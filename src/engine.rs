use crate::{
    closure, contain, context, execute, flow, leaf, machine, preflight, preset, protocol, sha256,
    state, template, watch, workspace,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct RunOpts {
    pub flow_path: PathBuf,
    pub vars: Vec<(String, String)>,
    pub emit: Option<String>,
    pub runs_dir: Option<PathBuf>,
    pub dry_run: bool,
    pub verbose: bool,
    pub quiet: bool,
    /// Continue a previous run directory instead of starting fresh.
    pub resume: Option<PathBuf>,
    pub resume_latest: bool,
    /// Allow resuming even though the flow file changed since that run.
    pub force_resume: bool,
    /// Suppress printing the best available output when the flow fails.
    pub no_partial_emit: bool,
    /// Re-launch this run outside the caller's process group / job object,
    /// print the run dir and exit, so a parent agent need not stay attached.
    pub detach: bool,
    /// Use exactly this run directory instead of a fresh timestamped one.
    /// Set by `--detach` when it hands the run off to the background copy.
    pub run_dir: Option<PathBuf>,
    /// Root for state that is not a run artifact - above all a managed
    /// workspace, which cannot live inside the repository it branches from.
    pub state_dir: Option<PathBuf>,
    /// Profile overlay files, applied in order (later wins).
    pub profiles: Vec<PathBuf>,
    /// Answer with a machine envelope on stdout instead of human progress.
    pub as_json: bool,
    /// `plan --save [dir]`: keep the rendered artifacts for review.
    pub save_plan: Option<Option<PathBuf>>,
    /// Accept the workspace's CURRENT contents as a new checkpoint on resume.
    /// Deliberately separate from --force-resume: one says "the definition of
    /// this run changed and I mean it", the other says "the working tree
    /// changed and I mean it", and conflating them made a single flag waive
    /// two unrelated guarantees.
    pub adopt_workspace: bool,
    /// Start a FRESH run that inherits an earlier run's spend: its visit
    /// counts, total leaf runs, reported cost and active time.
    ///
    /// This is the case a resume cannot serve, by design. When a run stops and
    /// the diagnosis is "the flow itself was wrong", fixing the flow is the
    /// correct response - and it invalidates the effective-config fingerprint,
    /// so `--resume` refuses, exactly as it should. What was left was a fresh
    /// run whose counters all started at zero, so the budget already spent
    /// simply vanished, and the only way to account for it was to edit the
    /// ceilings in the flow by hand. Hand arithmetic is not accounting: it is
    /// unverifiable, it is wrong the moment anyone loses count, and it leaves
    /// no record that the second run was ever a continuation of the first.
    pub carry_budget_from: Option<PathBuf>,
}

impl Default for RunOpts {
    fn default() -> Self {
        RunOpts {
            flow_path: PathBuf::new(),
            vars: Vec::new(),
            emit: None,
            runs_dir: None,
            dry_run: false,
            verbose: false,
            quiet: false,
            resume: None,
            resume_latest: false,
            force_resume: false,
            no_partial_emit: false,
            detach: false,
            run_dir: None,
            state_dir: None,
            profiles: Vec::new(),
            as_json: false,
            save_plan: None,
            adopt_workspace: false,
            carry_budget_from: None,
        }
    }
}

/// What one run inherited from an earlier one, and from where.
///
/// Only spend is carried: counters, not results. Step outputs, sessions, the
/// routing position and the workspace are all deliberately left behind, because
/// the flow that produced them is not the flow about to run - that is the whole
/// reason `--resume` refused and this exists.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CarriedBudget {
    pub from: PathBuf,
    /// Highest visit number reached per step id - the same shape `max_visits`
    /// is enforced against, so a loop that had four laps left really has four
    /// laps left. Only ids the NEW flow still defines are applied.
    pub visits: BTreeMap<String, u32>,
    /// Step ids the ancestor had visited that the corrected flow no longer
    /// defines. Their laps cannot be enforced against anything, so they are
    /// named rather than silently forgotten.
    pub dropped: Vec<String>,
    pub steps: u32,
    pub cost_usd: f64,
    pub elapsed_sec: u64,
}

impl CarriedBudget {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "from": self.from.display().to_string(),
            "visits": self.visits,
            "dropped_steps": self.dropped,
            "steps": self.steps,
            "cost_usd": self.cost_usd,
            "elapsed_sec": self.elapsed_sec,
        })
    }

    /// One line a human can check against the earlier run's own `runs why`.
    fn summary(&self) -> String {
        let laps = self
            .visits
            .iter()
            .map(|(step, visit)| format!("{step}@{visit}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut s = format!(
            "carried from {}: {} step run(s), ${:.4}, {}s active{}{}",
            self.from.display(),
            self.steps,
            self.cost_usd,
            self.elapsed_sec,
            if laps.is_empty() { "" } else { ", visits " },
            laps
        );
        if !self.dropped.is_empty() {
            // Never silent: a dropped id means that step's laps are NOT being
            // counted against anything in this run.
            s.push_str(&format!(
                " (not applied, no longer in the flow: {})",
                self.dropped.join(", ")
            ));
        }
        s
    }
}

pub fn run(opts: RunOpts) -> i32 {
    match run_inner(&opts) {
        Ok(code) => code,
        Err(e) => {
            // A config or usage failure is exactly the case a machine caller
            // cannot recover from by parsing prose, and exactly the case most
            // likely to happen when something is wrong - so JSON mode answers
            // with an envelope here too, never with a bare message.
            if opts.as_json {
                machine::emit(&machine::error_envelope(
                    if opts.dry_run { "plan" } else { "run" },
                    run_failure_code(&e),
                    &e,
                    2,
                    json!({
                        "state": "config_error",
                        "terminal": true,
                        "flow": abs(&opts.flow_path).display().to_string(),
                    }),
                ));
            } else {
                eprintln!("sfh: {e}");
            }
            2
        }
    }
}

pub fn validate(path: &Path, var_overrides: &[(String, String)]) -> i32 {
    validate_with_options(path, var_overrides, false, false, &[])
}

pub fn validate_with_options(
    path: &Path,
    var_overrides: &[(String, String)],
    strict: bool,
    as_json: bool,
    profiles: &[PathBuf],
) -> i32 {
    let inner = || -> Result<(flow::Flow, Vec<String>), String> {
        let flow = flow::load_with_overlays(path, profiles)?;
        let mut vars = flow.vars_string_map()?;
        for (k, v) in var_overrides {
            vars.insert(k.clone(), v.clone());
        }
        precheck(&flow, &vars, &HashSet::new())?;
        let mut warnings = flow::strict_warnings(&flow);
        // A workspace the flow cannot resolve is a static error, not a runtime
        // surprise: two writers sharing one workspace is refused here, before
        // anyone has paid for a step.
        match flow.workspace_plan() {
            Ok(plan) => warnings.extend(plan.warnings.iter().cloned()),
            Err(e) => return Err(e),
        }
        flow.context_plan(path)?;
        // Replay is a warning rather than an error: `rerun` has always been the
        // behaviour, and a flow that never crashes never notices. `--strict` is
        // where a user asks to be told about the choices they did not make.
        warnings.extend(flow.replay_warnings());
        if strict && !warnings.is_empty() {
            return Err(format!(
                "strict validation found {} issue(s):\n  - {}",
                warnings.len(),
                warnings.join("\n  - ")
            ));
        }
        Ok((flow, warnings))
    };
    match inner() {
        Ok((flow, warnings)) => {
            if as_json {
                let steps: Vec<_> = flow
                    .steps
                    .iter()
                    .map(|s| {
                        json!({
                            "id": s.id,
                            "kind": describe_kind(&flow, s),
                            "children": s.parallel.as_ref().map(|children| {
                                children.iter().map(|c| json!({
                                    "id": c.id,
                                    "kind": describe_kind(&flow, c),
                                })).collect::<Vec<_>>()
                            }).unwrap_or_default(),
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "ok": true,
                        "path": path.display().to_string(),
                        "strict": strict,
                        "api_version": flow.api_version.unwrap_or(1),
                        "warnings": warnings,
                        "steps": steps,
                    }))
                    .unwrap_or_default()
                );
            } else {
                eprintln!("OK: {} ({} steps)", path.display(), flow.steps.len());
                for warning in &warnings {
                    eprintln!("  warning: {warning}");
                }
                for s in &flow.steps {
                    eprintln!("  - {} ({})", s.id, describe_kind(&flow, s));
                    if let Some(children) = &s.parallel {
                        for c in children {
                            eprintln!("      * {} ({})", c.id, describe_kind(&flow, c));
                        }
                    }
                }
            }
            0
        }
        Err(e) => {
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "ok": false,
                        "path": path.display().to_string(),
                        "strict": strict,
                        "error": e,
                    }))
                    .unwrap_or_default()
                );
            } else {
                eprintln!("sfh: {e}");
            }
            2
        }
    }
}

pub fn show_config(path: &Path, show_secrets: bool, profiles: &[PathBuf]) -> i32 {
    match flow::load_with_overlays(path, profiles)
        .and_then(|flow| flow.effective_config_json_pretty(show_secrets))
    {
        Ok(config) => {
            println!("{config}");
            0
        }
        Err(e) => {
            eprintln!("sfh: {e}");
            2
        }
    }
}

pub fn graph(path: &Path, mermaid: bool) -> i32 {
    let flow = match flow::load(path) {
        Ok(flow) => flow,
        Err(e) => {
            eprintln!("sfh: {e}");
            return 2;
        }
    };
    if mermaid {
        // Step ids may contain '-' which is valid YAML for sfh but is not a
        // safe unquoted Mermaid node id. Keep user ids as labels and use a
        // stable, generated identifier for graph syntax.
        let nodes: HashMap<&str, String> = flow
            .steps
            .iter()
            .enumerate()
            .map(|(i, step)| (step.id.as_str(), format!("n{i}")))
            .collect();
        let terminal_node = |terminal: &str| format!("terminal_{terminal}");
        println!("flowchart TD");
        println!(
            "  flow_start([start]) --> {}",
            nodes[flow.steps[0].id.as_str()]
        );
        for (i, step) in flow.steps.iter().enumerate() {
            let node = &nodes[step.id.as_str()];
            let shape = if step.is_group() || step.is_foreach() {
                format!("{node}{{\"{}\"}}", step.id)
            } else {
                format!("{node}[\"{}\"]", step.id)
            };
            println!("  {shape}");
            for (rule, route) in step.route.iter().enumerate() {
                let target = nodes
                    .get(route.goto.as_str())
                    .cloned()
                    .unwrap_or_else(|| terminal_node(&route.goto));
                let label = if route.is_catch_all() {
                    "else".to_string()
                } else {
                    format!("route {rule}")
                };
                println!("  {node} -->|\"{label}\"| {target}");
            }
            if !step.route.iter().any(flow::Route::is_catch_all) {
                let target = flow
                    .steps
                    .get(i + 1)
                    .map(|s| nodes[s.id.as_str()].clone())
                    .unwrap_or_else(|| terminal_node("end"));
                println!("  {node} -. \"fallthrough\" .-> {target}");
            }
            for (kind, action) in [
                ("error", step.on_error.as_deref()),
                ("max visits", step.on_max_visits.as_deref()),
            ] {
                if let Some(action) = action {
                    let target = match action {
                        "continue" => flow
                            .steps
                            .get(i + 1)
                            .map(|next| nodes[next.id.as_str()].clone())
                            .unwrap_or_else(|| terminal_node("end")),
                        "fail" => terminal_node("fail"),
                        action => {
                            let target = action.strip_prefix("goto:").unwrap_or(action);
                            nodes
                                .get(target)
                                .cloned()
                                .unwrap_or_else(|| terminal_node(target))
                        }
                    };
                    println!("  {node} -->|\"{kind}: {action}\"| {target}");
                }
            }
        }
        if let Some(target) = flow.defaults.budget_goto() {
            let target = nodes
                .get(target)
                .cloned()
                .unwrap_or_else(|| terminal_node(target));
            println!("  budget_guard{{\"budget threshold (global)\"}}");
            println!("  budget_guard -->|\"on_budget\"| {target}");
        }
        for terminal in flow::TERMINALS {
            println!("  {}([\"{terminal}\"])", terminal_node(terminal));
        }
    } else {
        println!("flow: {}", path.display());
        for (i, step) in flow.steps.iter().enumerate() {
            let mut edges = Vec::new();
            for (rule, route) in step.route.iter().enumerate() {
                edges.push(format!(
                    "{} -> {}",
                    if route.is_catch_all() {
                        "else".into()
                    } else {
                        format!("route[{rule}]")
                    },
                    route.goto
                ));
            }
            let fallthrough = flow
                .steps
                .get(i + 1)
                .map(|s| s.id.as_str())
                .unwrap_or("end");
            if !step.route.iter().any(flow::Route::is_catch_all) {
                edges.push(format!("fallthrough -> {fallthrough}"));
            }
            for (kind, action) in [
                ("on_error", step.on_error.as_deref()),
                ("on_max_visits", step.on_max_visits.as_deref()),
            ] {
                if let Some(action) = action {
                    edges.push(format!(
                        "{kind} -> {}",
                        if action == "continue" {
                            fallthrough
                        } else {
                            action.strip_prefix("goto:").unwrap_or(action)
                        }
                    ));
                }
            }
            println!("  {}: {}", step.id, edges.join(", "));
        }
        if let Some(target) = flow.defaults.budget_goto() {
            println!("  on_budget (global) -> {target}");
        }
        for warning in flow::strict_warnings(&flow) {
            println!("  warning: {warning}");
        }
    }
    0
}

fn describe_kind(flow: &flow::Flow, s: &flow::Step) -> String {
    if s.is_group() {
        return format!(
            "parallel x{}",
            s.parallel.as_ref().map(|c| c.len()).unwrap_or(0)
        );
    }
    let base = if s.cmd.is_some() {
        "cmd".to_string()
    } else {
        leaf::effective(flow, s)
            .ok()
            .and_then(|e| e.tool)
            .unwrap_or_else(|| "?".into())
    };
    if s.is_foreach() {
        format!("foreach, {base}")
    } else {
        base
    }
}

/// Render every template once with empty step outputs so typos surface before
/// any expensive agent runs. Also checks profile/access resolution, and applies
/// the executed-privileged template rules (bin / cwd / argv[0] / shell-wrapped
/// cmd text) with the run's tainted-var set, so a resumed run dir's vars are
/// refused in those fields before any step spends anything (rev_break #12).
fn precheck(
    flow: &flow::Flow,
    vars: &BTreeMap<String, String>,
    tainted_vars: &HashSet<String>,
) -> Result<(), String> {
    let step_ids = flow.step_ids();
    let outputs = BTreeMap::new();
    let mut all: Vec<&flow::Step> = Vec::new();
    for s in &flow.steps {
        all.push(s);
        if let Some(children) = &s.parallel {
            all.extend(children.iter());
        }
    }
    let mk_builtins = |with_item: bool| {
        let mut b = BTreeMap::new();
        for k in [
            "run_dir",
            "flow_dir",
            "step_id",
            "visit",
            "os",
            "prompt_file",
            "notes",
            // v1.2's named context. The runtime defines both unconditionally
            // (empty when the step named no context), and `context_delivery:
            // file` is documented as "point the prompt at {{context_file}}" -
            // which validate and run were both rejecting outright, because
            // this list never learned about them.
            "context",
            "context_file",
            // The F5 budget snapshot. Listed here as well as in make_builtins
            // because this check is what decides whether `sfh validate`
            // accepts a key, and a validator that rejects what the runtime
            // accepts is as wrong as the other way round.
            "budget.spent_usd",
            "budget.elapsed_sec",
            "budget.remaining_usd",
            "budget.remaining_sec",
        ] {
            b.insert(k.to_string(), String::new());
        }
        if with_item {
            b.insert("item".to_string(), String::new());
            b.insert("item_index".to_string(), String::new());
        }
        b
    };
    for s in all {
        // {{item}}/{{item_index}} exist only for the body of a foreach step;
        // foreach.from and route conditions render WITHOUT them at runtime.
        let ctx_base = template::Ctx {
            vars,
            outputs: &outputs,
            step_ids: &step_ids,
            builtins: mk_builtins(false),
        };
        let ctx_item = template::Ctx {
            vars,
            outputs: &outputs,
            step_ids: &step_ids,
            builtins: mk_builtins(true),
        };
        let body_ctx = if s.is_foreach() { &ctx_item } else { &ctx_base };
        let chk = |ctx: &template::Ctx, label: &str, text: &str| -> Result<(), String> {
            template::render(text, ctx)
                .map(|_| ())
                .map_err(|e| format!("step '{}' {label}: {e}", s.id))
        };
        if let Some(p) = &s.prompt {
            chk(body_ctx, "prompt", p)?;
        }
        // Same privilege rules prepare_leaf applies at spawn time, repeated here
        // so the failure surfaces before any step runs (rev_break #12/#13).
        let exec_chk = |ctx: &template::Ctx, label: &str, text: &str| -> Result<(), String> {
            if s.allow_dynamic_exec_paths.unwrap_or(false) {
                return chk(ctx, label, text);
            }
            template::render_checked(text, ctx, &leaf::exec_template_check(tainted_vars))
                .map(|_| ())
                .map_err(|e| format!("step '{}' {label}: {e}", s.id))
        };
        let shell_chk = |ctx: &template::Ctx, text: &str| -> Result<(), String> {
            // flow.legacy_resume: the lenient loader already warned about this
            // exact template and let it through so a 0.x run could be resumed.
            // Refusing it again here made that warning a lie - the resume died
            // one step later. The METACHARACTER check still applies; only the
            // blanket "no templates in shell text" rule is relaxed.
            if s.unsafe_shell_template.unwrap_or(false) || flow.legacy_resume {
                template::render_checked(text, ctx, &leaf::shell_metachar_check)
                    .map(|_| ())
                    .map_err(|e| format!("step '{}' cmd: {e}", s.id))
            } else {
                template::render_checked(text, ctx, &|key, _| {
                    Err(leaf::shell_expansion_refused("string-form cmd", key))
                })
                .map(|_| ())
                .map_err(|e| format!("step '{}' cmd: {e}", s.id))
            }
        };
        match &s.cmd {
            Some(flow::Cmd::Shell(c)) => shell_chk(body_ctx, c)?,
            Some(flow::Cmd::Argv(v)) => {
                if let Some(first) = v.first() {
                    exec_chk(body_ctx, "cmd[0]", first)?;
                }
                let script_span = leaf::shell_script_span(v);
                for (i, c) in v.iter().enumerate().skip(1) {
                    match &script_span {
                        Some(span) if span.contains(&i) => shell_chk(body_ctx, c)?,
                        _ => chk(body_ctx, "cmd", c)?,
                    }
                }
            }
            None => {}
        }
        if let Some(f) = &s.foreach {
            chk(&ctx_base, "foreach.from", &f.from)?;
        }
        for r in &s.route {
            // EVERY predicate whose text is template-rendered at routing time
            // belongs here. when_stderr_matches was missed when it was added:
            // its template was rendered only in evaluate_route, so a typo in it
            // passed validate and dry-run and killed the run after the step it
            // guards had already been executed and billed - the one thing this
            // function exists to prevent.
            for t in [
                &r.when_contains,
                &r.when_matches,
                &r.when_last_line_contains,
                &r.when_last_line_is,
                &r.when_last_line_matches,
                &r.when_stderr_matches,
                &r.when_label_is,
            ]
            .into_iter()
            .flatten()
            {
                chk(&ctx_base, "route condition", t)?;
            }
            if let Some(wm) = &r.when_members {
                chk(&ctx_base, "route condition", &wm.last_line_is)?;
            }
        }
        if let Some(c) = &s.compact {
            if let Some(i) = &c.instruction {
                chk(body_ctx, "compact.instruction", i)?;
            }
        }
        if !s.is_group() {
            // Check the MERGED values (step > profile > defaults) so templates
            // supplied by a profile or defaults are validated too.
            let variants: Vec<Option<&str>> = std::iter::once(None)
                .chain(s.fallback.iter().map(|f| Some(f.as_str())))
                .collect();
            for ov in variants {
                let eff = leaf::effective_with(flow, s, ov)?;
                for (label, v) in [
                    ("model", &eff.model),
                    ("effort", &eff.effort),
                    ("agent", &eff.agent),
                ] {
                    if let Some(t) = v {
                        chk(body_ctx, label, t)?;
                    }
                }
                // bin / cwd are executed-privileged: apply the run-derived
                // template refusal (with this run's tainted vars), not a plain
                // render (rev_break #12).
                for (label, v) in [("bin", &eff.bin), ("cwd", &eff.cwd)] {
                    if let Some(t) = v {
                        exec_chk(body_ctx, label, t)?;
                    }
                }
                // args may render into permission flags (vars are known now;
                // step outputs render empty here and are re-checked right
                // before each spawn). The whole rendered argv is checked at
                // once so flag+value pairs are read together. Fail-closed like
                // validation.
                let mut rendered_args = Vec::with_capacity(eff.args.len());
                for a in &eff.args {
                    rendered_args.push(
                        template::render(a, body_ctx)
                            .map_err(|e| format!("step '{}' args: {e}", s.id))?,
                    );
                }
                if eff.access != preset::Access::Full && !s.allow_access_override.unwrap_or(false) {
                    if let Some(t) = &eff.tool {
                        if let Some(e) = preset::find_escalation(t, eff.access, &rendered_args) {
                            return Err(preset::escalation_error(&s.id, eff.access, &e));
                        }
                    }
                }
                for v in eff.env.values() {
                    chk(body_ctx, "env", v)?;
                }
                if let (Some(tool), Some(e)) = (&eff.tool, &eff.effort) {
                    if !e.contains("{{") {
                        if let Some(w) = effort_vocab_warning(tool, e) {
                            eprintln!("sfh: warning: step '{}': {w}", s.id);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Effort level names differ per CLI; warn early instead of failing mid-flow.
fn effort_vocab_warning(tool: &str, e: &str) -> Option<String> {
    let known: &[&str] = match tool {
        "codex" => &[
            "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ],
        "claude" => &["low", "medium", "high", "xhigh", "max", "ultracode"],
        "grok" => &["low", "medium", "high"],
        "agy" => &["low", "medium", "high"],
        // pi warns and silently falls back to its default on an unknown level.
        "pi" => &["off", "minimal", "low", "medium", "high", "xhigh", "max"],
        // opencode: per-model variants; cursor: effort lives in the model slug
        _ => return None,
    };
    if known.contains(&e) {
        None
    } else {
        Some(format!(
            "effort '{e}' is not a known {tool} level ({}) - it may be rejected or silently ignored",
            known.join("/")
        ))
    }
}

/// How a flow body finished when it did not error out.
///
/// `Stuck` is the third terminal: the run did the work it could and a step the
/// user marked `goto: stuck` was reached, so the result is neither a success
/// (exit 0 would tell a parent agent to move on) nor a failure (exit 1 would
/// say the plumbing broke). It exits 4, keeps the partial output a failure
/// would emit, and stays resumable.
enum FlowEnd {
    Completed,
    /// The step whose routing decision landed on `stuck`.
    Stuck {
        after: String,
    },
}

enum ErrorDisposition {
    Continue,
    Goto(usize),
    Completed,
    Stuck,
}

/// The `on_budget` landing (F5): where to jump, and the point on each axis at
/// which to jump there. Threshold = ceiling - reserve, cost and wall-clock
/// independently, so a run gets a wrap-up chain BEFORE the ceiling that would
/// otherwise end it with an error and nothing to hand back.
///
/// The ceiling checks stay exactly as they were and keep running afterwards:
/// the reserve is headroom, not an extension, and a landing chain that eats it
/// too still ends the run the hard way (fail-closed preservation).
struct BudgetPlan {
    /// Step id or terminal, already stripped of the `goto:` prefix.
    goto: String,
    /// Reported spend at which to land. None when no cost ceiling is declared.
    cost_at: Option<f64>,
    /// Moment at which to land. None when no wall-clock ceiling is declared.
    wall_at: Option<Instant>,
}

impl BudgetPlan {
    fn of(
        defaults: &flow::Defaults,
        flow_start: Instant,
        elapsed_before_attempt: u64,
    ) -> Option<Self> {
        let goto = defaults.budget_goto()?.to_string();
        // A reserve larger than its own ceiling clamps to "land at once"
        // rather than wrapping around into a threshold in the past/negative.
        // That is the honest reading of "keep 20 minutes of a 10 minute
        // budget": there was never room for the work in the first place.
        let reserve_usd = defaults.budget_reserve_usd();
        let reserve_sec = defaults.budget_reserve_sec();
        Some(Self {
            goto,
            cost_at: defaults.max_cost_usd.map(|m| (m - reserve_usd).max(0.0)),
            wall_at: defaults.wall_clock_sec.map(|s| {
                flow_start
                    + Duration::from_secs(
                        s.saturating_sub(reserve_sec)
                            .saturating_sub(elapsed_before_attempt),
                    )
            }),
        })
    }

    /// Which axis (if any) has crossed its threshold. Wall-clock is asked
    /// first only so the answer is stable when both cross at once.
    fn trigger(&self, now: Instant, cost_usd: f64) -> Option<&'static str> {
        if self.wall_at.is_some_and(|t| now > t) {
            return Some("wall_clock");
        }
        if self.cost_at.is_some_and(|c| cost_usd >= c) {
            return Some("cost");
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Run state that survives a crash: everything needed by --resume.
// ---------------------------------------------------------------------------

fn failed_output(step: &str, text: &str, exit: i32, timed_out: bool) -> String {
    format!(
        "[sfh: step '{step}' did not complete (exit={exit}, timed_out={timed_out}).\n \
         The text below is whatever it produced before failing. It is not a result.]\n{text}"
    )
}

fn claim_leaf_runs(
    total: &mut u32,
    additional: u32,
    max_total: u32,
    step: &str,
) -> Result<(), String> {
    let next = total.checked_add(additional).ok_or_else(|| {
        format!(
            "step '{step}' would overflow the total leaf-run counter (max_total_steps={max_total})"
        )
    })?;
    if next > max_total {
        return Err(format!(
            "step '{step}' would bring total leaf runs to {next} over max_total_steps ({max_total})"
        ));
    }
    *total = next;
    Ok(())
}

fn accumulate_cost(total: &mut f64, additional: f64) {
    // Provider reports and resumed logs are accounting input, not trusted
    // arithmetic. They cannot refund spend, and summing two individually finite
    // f64::MAX values must saturate instead of producing Infinity (which JSON
    // cannot represent and which would otherwise panic status serialization).
    let additional = if additional.is_nan() || additional < 0.0 {
        0.0
    } else if additional.is_infinite() {
        f64::MAX
    } else {
        additional
    };
    let sum = total.max(0.0) + additional;
    *total = if sum.is_finite() { sum } else { f64::MAX };
}

#[derive(Clone)]
struct PendingRoute {
    step: String,
    visit: u32,
    route_text: String,
    /// The recorded step completed with failure. Resume must replay on_error
    /// before considering ordinary routes, exactly as the live path does.
    errored: bool,
    /// True when route_text came from a fan-out's headerless plain output.
    /// Compaction rewrites the chain file but never changes what live routing
    /// matched against, so a plain-sourced route must NOT be patched from the
    /// precompact file the way a leaf's chain-sourced route is.
    from_plain: bool,
    /// The aggregate_end member snapshot, for a `when_members` rule to count.
    /// None on a leaf's route (nobody to count) and on a fan-out recorded
    /// before the snapshot existed - the router tells those two apart from the
    /// flow, and refuses the second rather than guessing (see `Members`).
    members: Option<Vec<MemberVerdict>>,
    /// Durable protocol evidence for a leaf. None for fan-out composites and
    /// logs written before protocol_state was recorded.
    protocol: Option<protocol::ProtocolState>,
    /// The step/aggregate result is durable, but compact/notes post-processing
    /// has not yet reached its own durable end marker. Resume must finish that
    /// stage before evaluating this route.
    postprocess: bool,
    /// A compact_end/compact_failed event points at a durable compacted (or
    /// head+tail fallback) chain. Resume skips the paid summarizer when this is
    /// true and continues with the next post-processing stage.
    compact_done: bool,
    /// The visit's notes section has been atomically published and notes_end
    /// was recorded. Resume must not append it again.
    notes_done: bool,
}

/// The per-member snapshot an aggregate_end carries, or None when the event has
/// none (a run from before they were recorded) or contradicts itself.
///
/// All-or-nothing on purpose. Skipping one unreadable entry and keeping the
/// rest would shrink the denominator, and a smaller denominator is exactly how
/// `all: true` starts passing on a group that did not agree. None sends the
/// router down the "cannot answer this" path instead.
fn restored_members(v: &serde_json::Value) -> Option<Vec<MemberVerdict>> {
    let arr = v.get("members")?.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for m in arr {
        let id = m.get("id")?.as_str()?.to_string();
        let exit = i32::try_from(m.get("exit")?.as_i64()?).ok()?;
        let last_line = m.get("last_line")?.as_str()?.to_string();
        out.push(MemberVerdict {
            id,
            // Both halves must agree before a member votes. log.jsonl is
            // rewritable in a run dir on --resume, and trusting "ok" alone
            // would promote a member that exited 7 to a voter by editing one
            // word - the same fail-closed reading the step_end restore uses.
            ok: m.get("ok")?.as_bool()? && exit == 0,
            exit,
            // Re-cut: the writer already did, but a hand-edited longer value
            // must not be comparable to something the live path could not have
            // produced.
            last_line: clip(&last_line, ROUTE_LINE_CHARS),
            // Either the writer said it cut the line, or the recorded value is
            // longer than the writer could have written - both mean the value
            // being compared is a prefix. Missing key = an unclipped line, the
            // shape every ordinary verdict has.
            clipped: m.get("clipped").and_then(|c| c.as_bool()).unwrap_or(false)
                || last_line.chars().count() > ROUTE_LINE_CHARS,
        });
    }
    Some(out)
}

fn restored_visit(v: &serde_json::Value, step: &str) -> Result<u32, String> {
    let Some(raw) = v.get("visit") else {
        // Pre-visit logs represented the first pass implicitly.
        return Ok(1);
    };
    let value = raw
        .as_u64()
        .ok_or_else(|| format!("resume: event for step '{step}' records a non-integer visit"))?;
    u32::try_from(value).map_err(|_| {
        format!("resume: event for step '{step}' records visit {value} outside the supported range")
    })
}

#[derive(Clone)]
struct UnfinishedStep {
    step: String,
    visit: u32,
    started: String,
    cmd: String,
    /// A durable step_end selected this fallback, but its own final step_end
    /// was never recorded. Resume this profile directly in the same visit.
    profile: Option<String>,
}

#[derive(Default)]
struct ResumeState {
    outputs: BTreeMap<String, template::StepOutput>,
    visits: HashMap<String, u32>,
    sessions: HashMap<String, leaf::SessionInfo>,
    chain_files: HashMap<String, PathBuf>,
    total: u32,
    cost_usd: f64,
    /// Active run time recorded by the last status heartbeat. A resume carries
    /// this forward so wall_clock_sec is a run budget, not a per-attempt budget.
    elapsed_sec: u64,
    start: Option<String>,
    pending_route: Option<PendingRoute>,
    unfinished_step: Option<UnfinishedStep>,
    last_executed: Option<String>,
    last_success: Option<String>,
    completed: bool,
    /// The last durable workspace fingerprint this run recorded. A resume
    /// compares the workspace against it to tell "nothing moved" from "something
    /// outside this run edited it". Absent for a run with no managed workspace,
    /// and for every run written before v1.2.
    workspace_checkpoint: Option<String>,
    /// True once this run has already spent its one `on_budget` landing. The
    /// log is the only record of it: without this the resumed run would arrive
    /// with the restored cost still over the threshold and land a second time,
    /// which turns "one wrap-up chain per run" into "one per crash".
    budget_landed: bool,
    /// A retry backoff reached the wall-clock landing threshold after its last
    /// attempt was durably recorded. Kept until budget_landing appears so a
    /// crash in that narrow gap resumes the landing instead of on_error.
    retry_budget_trigger: Option<String>,
    /// Fan-out members that already finished in a crashed attempt, keyed by
    /// (parent step id, visit). A resume that re-runs a parallel/foreach group
    /// SKIPS these instead of executing them a second time: re-running spent
    /// money twice, opened duplicate sessions, and could push the restored
    /// count plus a full fresh batch past max_total_steps, wedging the resume
    /// (rev_regression: fan-out members re-executed after a mid-group crash).
    completed_members: HashMap<(String, u32), HashSet<String>>,
    /// A failed fan-out member durably selected this profile but stopped
    /// before that fallback reached step_end. Keyed separately per member so a
    /// resumed group can continue the paid attempt chain instead of starting
    /// that member's primary profile again.
    member_fallbacks: HashMap<(String, u32, String), String>,
    /// Ordered foreach input fingerprints recorded when the group opened.
    /// Completed items are keyed by index, so their meaning is safe to restore
    /// only while the exact ordered item list is unchanged.
    foreach_inputs: HashMap<(String, u32), String>,
}

#[cfg(test)]
fn load_resume(run_dir: &Path) -> Result<ResumeState, String> {
    load_resume_for_flow(run_dir, None)
}

fn load_resume_for_flow(
    run_dir: &Path,
    current_flow: Option<&flow::Flow>,
) -> Result<ResumeState, String> {
    // Contained, no-follow read: log.jsonl is a fixed name in a directory an
    // attacker controls on --resume, and a symlink there used to be followed
    // to an external JSONL file that was then ingested as the run's entire
    // restored state (rev_break #6). A missing log is a hard error (the caller
    // already verified the dir looks like a run).
    let log = contain::read_contained_opt(run_dir, "log.jsonl")?
        .ok_or_else(|| format!("cannot read {}/log.jsonl: file missing", run_dir.display()))?;
    let mut st = ResumeState::default();
    // Active seconds this run INHERITED from an earlier one. Kept separate
    // because the ordinary source of elapsed_sec is status.json, and that file
    // is not durable the way the log is - a detached resume deletes it and
    // seeds a zeroed replacement. Used as a FLOOR at the end of this function,
    // never as an addend, so it can restore a lost figure without ever
    // double-counting a present one.
    let mut carried_elapsed: u64 = 0;
    // The most durable elapsed-time source there is (see the restore
    // precedence at the end of this function): a `run_end` event is logged
    // exactly once, at the true end of an attempt, and never rewritten
    // afterwards - unlike status.json, which a detached resume deletes and
    // reseeds at zero. A log spanning several attempts (a stuck run,
    // resumed, then failing) can carry more than one of these; the latest
    // describes the most recent attempt, so it wins (plain assignment, not
    // a max: an attempt's own total already includes everything before it).
    let mut run_end_elapsed: Option<u64> = None;
    let mut last_step: Option<String> = None;
    let mut unfinished: BTreeMap<(String, u32), UnfinishedStep> = BTreeMap::new();
    // A completed leaf's chain may later be intentionally replaced by its
    // compact substage. Defer a mismatched step_end hash until the log proves
    // that transition and verify the pre-compact recovery artifact instead.
    let mut pending_step_hashes: HashMap<(String, u32), String> = HashMap::new();
    // Groups whose finished members a resume should carry into the next visit,
    // and the visit to carry them FROM. Populated when a fan-out's lap ends in
    // failure, and removed the moment the log shows the run did something else
    // with that group afterwards - routed back into it, or opened a new lap.
    //
    // Read from the log in order rather than reconstructed at the end. Four
    // attempts at inferring this from the finished state (highest visit with
    // members, whether a visit was left open, whether the last lap failed) each
    // leaked a different case, because "the run stopped here" and "the flow
    // deliberately came back here" look identical once the log is a set of
    // facts. In sequence they do not: one is a failed aggregate_end with
    // nothing after it, the other has a position or a group_start after it.
    let mut carry_from: HashMap<String, u32> = HashMap::new();
    for line in log.lines() {
        // A malformed line is skipped, not a hard error: the log is append-only
        // and the last line is routinely torn when sfh is killed mid-write, so
        // refusing to resume over it would defeat crash recovery. This is
        // fail-SAFE, not fail-open: dropping a line removes that step's
        // completion record, which makes the step look UNFINISHED and re-run -
        // it can never fabricate a success. Success additionally requires a
        // positively recorded exit 0 (see the `ok` computation below), so an
        // attacker who corrupts a line gains nothing (rev_break #12).
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let ev = v.get("event").and_then(|x| x.as_str()).unwrap_or("");
        let step = v
            .get("step")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        match ev {
            // A child finished (and may already have been billed), but sfh
            // could not publish the complete artifact set that would make the
            // result safe to restore. Treat this run as permanently
            // non-resumable instead of interpreting the preceding step_start
            // as permission to execute the uncertain side effect again.
            "persistence_failure" => {
                let error = v
                    .get("error")
                    .and_then(|x| x.as_str())
                    .unwrap_or("required result artifacts could not be persisted");
                return Err(format!(
                    "resume: run is non-resumable after persistence failure in step '{step}': {error}; verify the external side effect before starting a new run"
                ));
            }
            // compact_start is also a recovery record: its precompact artifact
            // is the last known-good chain. The compacted chain is published
            // atomically before compact_end, so a kill in that narrow interval
            // leaves the new bytes on disk but no completion checkpoint. Reset
            // the in-memory result to the original here and rerun the unfinished
            // compactor; otherwise it would summarize its own summary and route
            // a leaf against text the live run never judged.
            "compact_start" => {
                let visit = restored_visit(&v, &step)?;
                if st
                    .pending_route
                    .as_ref()
                    .is_some_and(|pending| pending.step == step && pending.visit == visit)
                {
                    // Older log writers did not name the recovery artifact.
                    // Preserve their previous best-effort behavior, but a new
                    // checkpoint that names a missing artifact is corruption.
                    if let Some(path) = v.get("precompact_file").and_then(|x| x.as_str()) {
                        let original =
                            contain::read_contained_opt(run_dir, path)?.ok_or_else(|| {
                                format!(
                                    "resume: compact start for step '{step}' names missing artifact '{path}'"
                                )
                            })?;
                        if let Some(expected) = pending_step_hashes.remove(&(step.clone(), visit)) {
                            if !output_fingerprint_matches(&expected, &original) {
                                return Err(format!(
                                    "resume: compact recovery artifact for step '{step}' does not match the completed leaf output hash"
                                ));
                            }
                        }
                        let original = original.trim_end().to_string();
                        if let Some(output) = st.outputs.get_mut(&step) {
                            output.output = original.clone();
                            output.outputs = original.clone();
                        }
                        if let Some(pending) = st.pending_route.as_mut() {
                            if !pending.from_plain {
                                pending.route_text = original;
                            }
                        }
                    } else {
                        // Legacy compact_start records did not name a recovery
                        // artifact, so there is no byte source against which an
                        // old step hash can be checked.
                        pending_step_hashes.remove(&(step.clone(), visit));
                    }
                }
            }
            // Spend this run inherited from an earlier one via
            // --carry-budget-from. Folded in HERE, while reading the log in
            // order, so it is the baseline the run's own step_end events then
            // add to - and so it composes. Without this a second correction
            // would silently forget the first run's spend, which is precisely
            // the arithmetic the feature exists to stop anyone doing by hand.
            // The event sits immediately after run_start, before any step ran,
            // so the `.max(visit)` below can only ever raise these.
            "budget_carried" => {
                if let Some(n) = v.get("steps").and_then(|x| x.as_u64()) {
                    st.total = st.total.saturating_add(n.min(u32::MAX as u64) as u32);
                }
                if let Some(c) = v.get("cost_usd").and_then(|x| x.as_f64()) {
                    accumulate_cost(&mut st.cost_usd, c);
                }
                if let Some(visits) = v.get("visits").and_then(|x| x.as_object()) {
                    for (step, visit) in visits {
                        if let Some(n) = visit.as_u64() {
                            let e = st.visits.entry(step.clone()).or_insert(0);
                            *e = (*e).max(n.min(u32::MAX as u64) as u32);
                        }
                    }
                }
                // elapsed_sec is deliberately NOT ADDED: whichever of the
                // three sources wins in the restore precedence at the end of
                // this function (run_end, then meta.json, then status.json)
                // records elapsed_before_attempt PLUS this attempt, so the
                // carried seconds are normally already inside it, and adding
                // them would double-count. Remembered as a floor instead,
                // because "normally" is not "always": detach_run deletes a
                // resumed run's status.json and seeds a zeroed replacement,
                // so on `--resume <carried-run> --detach` the only surviving
                // record of the inherited seconds may be this event. Steps,
                // cost and visits are all durable in the log; wall clock has
                // to be too, or three of the four carried ceilings hold and
                // the fourth silently resets.
                if let Some(secs) = v.get("elapsed_sec").and_then(|x| x.as_u64()) {
                    carried_elapsed = carried_elapsed.max(secs);
                }
            }
            // A completed compactor is its own durable post-processing stage.
            // Restore its published chain so a crash before postprocess_end
            // does not launch and bill the summarizer a second time.
            "compact_end" | "compact_failed" => {
                // A live run counts the summarizer as one more leaf run and
                // adds its cost the moment compact starts; a resume that drops
                // both under-reports steps_done and cost, which can push a
                // resumed run past max_total_steps / max_cost_usd that the
                // live run would have honoured.
                st.total = st.total.saturating_add(1);
                if let Some(c) = v.get("cost_usd").and_then(|x| x.as_f64()) {
                    accumulate_cost(&mut st.cost_usd, c);
                }
                // A path that escapes the run dir fails the resume outright;
                // only a genuinely absent file reads as None (S1-4).
                let precompact = match v.get("precompact_file").and_then(|x| x.as_str()) {
                    Some(p) => contain::read_contained_opt(run_dir, p)?,
                    None => None,
                };
                let compact_file = v.get("chain_file").and_then(|x| x.as_str());
                let compacted = match compact_file {
                    Some(path) => Some(
                        contain::read_contained_opt(run_dir, path)?.ok_or_else(|| {
                            format!(
                                "resume: compact checkpoint for step '{step}' names missing artifact '{path}'"
                            )
                        })?,
                    ),
                    None => None,
                };
                if let (Some(expected), Some(compacted)) = (
                    v.get("output_hash").and_then(|x| x.as_str()),
                    compacted.as_deref(),
                ) {
                    if !output_fingerprint_matches(expected, compacted) {
                        return Err(format!(
                            "resume: compact checkpoint for step '{step}' does not match the recorded output hash"
                        ));
                    }
                }
                if let Some(e) = st.outputs.get_mut(&step) {
                    if let Some(original) = precompact.as_ref() {
                        e.outputs = original.trim_end().to_string();
                    }
                    if let Some(compacted) = compacted.as_ref() {
                        e.output = compacted.trim_end().to_string();
                    }
                }
                let visit = restored_visit(&v, &step)?;
                if let Some(pending) = st.pending_route.as_mut() {
                    if pending.step == step && pending.visit == visit {
                        pending.compact_done = compacted.is_some();
                    }
                    if pending.step == step && pending.visit == visit && !pending.from_plain {
                        // Live routing uses the pre-compact text, even though
                        // the chain file now contains the summary/head+tail.
                        // A fan-out's route text is its headerless plain
                        // output, which compaction never touches.
                        if let Some(original) = precompact {
                            pending.route_text = original.trim_end().to_string();
                        }
                    }
                }
                if let Some(path) = compact_file {
                    if let Some(canon) = contain::contained_opt(run_dir, path)? {
                        st.chain_files.insert(step.clone(), canon);
                    }
                }
            }
            // A MEMBER's step_start (it carries `parent`) is a lineage record
            // only. Tracking it as an unfinished step would hand the resume a
            // child id as the place to restart, which is not a top-level step;
            // the group's own group_start/foreach_start below already stands for
            // the whole fan-out.
            "step_start" if v.get("parent").is_some_and(|p| !p.is_null()) => {}
            "step_start" => {
                let visit = restored_visit(&v, &step)?;
                unfinished.insert(
                    (step.clone(), visit),
                    UnfinishedStep {
                        step: step.clone(),
                        visit,
                        started: v
                            .get("ts")
                            .and_then(|x| x.as_str())
                            .unwrap_or("unknown")
                            .to_string(),
                        cmd: v
                            .get("cmd")
                            .and_then(|x| x.as_str())
                            .unwrap_or("unknown command")
                            .to_string(),
                        profile: None,
                    },
                );
            }
            // A fan-out logs no step_start of its OWN (only its members do, and
            // those are skipped just above), so without these events a crash
            // mid-fan-out - before aggregate_end - leaves no record of where the
            // run was. A flow whose FIRST step is a fan-out then has nothing to
            // resume from at all. Track the group exactly like an unfinished
            // leaf; aggregate_end clears it.
            "group_start" | "foreach_start" => {
                let visit = restored_visit(&v, &step)?;
                // A new lap opened, so whatever the previous one left behind is
                // not something to carry into it.
                carry_from.remove(&step);
                let (kind, members) = if ev == "group_start" {
                    ("parallel", "children")
                } else {
                    ("foreach", "items")
                };
                let n = v.get(members).and_then(|x| x.as_u64()).unwrap_or(0);
                unfinished.insert(
                    (step.clone(), visit),
                    UnfinishedStep {
                        visit,
                        started: v
                            .get("ts")
                            .and_then(|x| x.as_str())
                            .unwrap_or("unknown")
                            .to_string(),
                        cmd: format!("{kind} fan-out ({n} members)"),
                        step: step.clone(),
                        profile: None,
                    },
                );
                if ev == "foreach_start" {
                    if let Some(hash) = v
                        .get("items_hash")
                        .and_then(|value| value.as_str())
                        .filter(|value| !value.is_empty())
                    {
                        let key = (step.clone(), visit);
                        if let Some(previous) = st.foreach_inputs.get(&key) {
                            if previous != hash {
                                return Err(format!(
                                    "resume: foreach step '{step}' visit {visit} has contradictory ordered-input fingerprints in log.jsonl"
                                ));
                            }
                        } else {
                            st.foreach_inputs.insert(key, hash.to_string());
                        }
                    }
                }
            }
            // A member an earlier resume carried over rather than executed. It
            // has no output, cost or session of its own to restore - those came
            // back with the ORIGINAL step_end, which is still in this same log -
            // so all this records is "do not run it again".
            "members_restored" => {
                if let Some(parent) = v
                    .get("parent")
                    .and_then(|p| p.as_str())
                    .filter(|p| !p.is_empty())
                {
                    let visit = restored_visit(&v, &step)?;
                    let entry = st
                        .completed_members
                        .entry((parent.to_string(), visit))
                        .or_default();
                    for name in v
                        .get("steps")
                        .and_then(|s| s.as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(|s| s.as_str())
                    {
                        entry.insert(name.to_string());
                    }
                }
            }
            "step_end" | "aggregate_end" => {
                if ev == "step_end" {
                    st.total = st.total.saturating_add(1);
                    if v.get("retry_budget_exhausted")
                        .and_then(|value| value.as_bool())
                        == Some(true)
                    {
                        st.retry_budget_trigger = Some("wall_clock".into());
                    }
                }
                // A run dir is untrusted on --resume: a NEGATIVE cost_usd in an
                // edited log would subtract from the running total and let a
                // resumed run slip under max_cost_usd. Reported cost can only be
                // spent, never refunded, so clamp at zero (rev_break #12).
                if let Some(c) = v.get("cost_usd").and_then(|x| x.as_f64()) {
                    accumulate_cost(&mut st.cost_usd, c);
                }
                let visit = restored_visit(&v, &step)?;
                let is_child = v.get("parent").is_some_and(|p| !p.is_null());
                let next_fallback = v
                    .get("next_fallback")
                    .and_then(|profile| profile.as_str())
                    .filter(|profile| !profile.is_empty())
                    .map(str::to_string);
                if ev == "step_end" {
                    if let Some(parent) = v
                        .get("parent")
                        .and_then(|parent| parent.as_str())
                        .filter(|parent| !parent.is_empty())
                    {
                        let key = (parent.to_string(), visit, step.clone());
                        if let Some(profile) = &next_fallback {
                            st.member_fallbacks.insert(key, profile.clone());
                        } else {
                            st.member_fallbacks.remove(&key);
                        }
                    }
                } else {
                    // aggregate_end closes every recovery stage owned by this
                    // group visit. Honest logs have already removed them one by
                    // one; this also keeps malformed stale entries from leaking
                    // into a later explicit lap.
                    st.member_fallbacks.retain(|(parent, member_visit, _), _| {
                        parent != &step || *member_visit != visit
                    });
                }
                let postprocess_pending = v
                    .get("postprocess_pending")
                    .and_then(|pending| pending.as_bool())
                    == Some(true);
                // step_end clears an unfinished leaf; aggregate_end clears the
                // fan-out group opened by group_start/foreach_start. A child's
                // step_end is keyed by the CHILD id, so it cannot clear its
                // parent group's entry.
                unfinished.remove(&(step.clone(), visit));
                if !is_child {
                    let e = st.visits.entry(step.clone()).or_insert(0);
                    *e = (*e).max(visit);
                    last_step = Some(step.clone());
                    st.last_executed = Some(step.clone());
                }
                // Fail-CLOSED on missing or mistyped success fields. The old
                // `.unwrap_or(0)` / `.unwrap_or(false)` let an edited log drop
                // `exit` (or set it to a non-integer), or drop timed_out /
                // interrupted, and have a failed step read back as a clean
                // success, which a resume then skipped instead of re-running.
                // A step is only "ok" when the log POSITIVELY records exit 0,
                // timed_out false and interrupted false; anything absent or
                // ambiguous is treated as not-ok and re-run (rev_break #12).
                // The two writers emit DIFFERENT field sets, so "fail closed on
                // a missing field" has to ask each event for the fields its own
                // writer produces: step_end carries exit/timed_out/interrupted,
                // aggregate_end carries exit/failed. Demanding the union marked
                // every honestly written aggregate_end as not-ok, which dropped
                // fan-out groups out of last_success and left a resumed run
                // emitting the wrong step at the end.
                //
                // Fields the event does not owe are still not allowed to
                // contradict: present-but-true, or present-but-mistyped, is
                // never a success.
                let exit_raw = v.get("exit").and_then(|x| x.as_i64());
                let timed_out_raw = v.get("timed_out").and_then(|x| x.as_bool());
                let interrupted_raw = v.get("interrupted").and_then(|x| x.as_bool());
                let failed_raw = v.get("failed").and_then(|x| x.as_bool());
                let absent_or_false =
                    |key: &str, parsed: Option<bool>| v.get(key).is_none() || parsed == Some(false);
                let owed_fields_false = if ev == "step_end" {
                    timed_out_raw == Some(false) && interrupted_raw == Some(false)
                } else {
                    failed_raw == Some(false)
                };
                // How this group's most recent lap ENDED, kept per group rather
                // than inferred later. Deciding whether a resume is continuing
                // an interrupted batch or starting a fresh lap by looking at
                // proxies - the highest visit that has completed members, or
                // whether some visit is still open - got the answer wrong three
                // times in three review rounds. The question is simply "did the
                // last aggregate_end for this group say failed", so record that.
                if ev == "aggregate_end" && !is_child {
                    if failed_raw == Some(false) {
                        carry_from.remove(&step);
                    } else {
                        carry_from.insert(step.clone(), visit);
                    }
                }
                let ok = exit_raw == Some(0)
                    && absent_or_false("timed_out", timed_out_raw)
                    && absent_or_false("interrupted", interrupted_raw)
                    && absent_or_false("failed", failed_raw)
                    && owed_fields_false;
                // A failed process result is still a completed result. If sfh
                // died after writing step_end/aggregate_end but before writing
                // the on_error position, resuming must replay that decision
                // rather than probe the external system a second time. Keep
                // malformed or interrupted records on the conservative re-run
                // path: every field that event type owes must be present and
                // typed, and an interrupted leaf has not completed its work.
                let exit_is_i32 = exit_raw.and_then(|x| i32::try_from(x).ok()).is_some();
                let completed_event = exit_is_i32
                    && if ev == "step_end" {
                        timed_out_raw.is_some() && interrupted_raw == Some(false)
                    } else {
                        failed_raw.is_some()
                    };
                // A SUCCESSFUL fan-out member: remember it under its PARENT
                // group so a resume that re-enters the group skips it instead
                // of spending money and sessions a second time (rev_regression:
                // completed members re-executed). The member key is exactly the
                // `step` the member logged under - a parallel child's id, or a
                // foreach item's "id[i]" label - and the group re-derives the
                // same keys when it rebuilds its batch.
                //
                // Gated on `ok`, and it must stay that way: the crash being
                // resumed from is usually ONE member failing, and recording
                // that member as complete is how a resume ends up skipping the
                // only step that still needs to run. It then finishes with the
                // failure text as the member's output and never retries it.
                if ok {
                    if let Some(parent) = v
                        .get("parent")
                        .and_then(|p| p.as_str())
                        .filter(|p| !p.is_empty())
                    {
                        st.completed_members
                            .entry((parent.to_string(), visit))
                            .or_default()
                            .insert(step.clone());
                    }
                }
                // A run dir is untrusted input on --resume: the artifact paths
                // recorded in the log must stay inside it, symlinks resolved.
                // A path pointing elsewhere used to be swallowed into empty
                // output; it now fails the whole resume instead (S1-4).
                let chain = match v.get("chain_file").and_then(|x| x.as_str()) {
                    Some(p) => contain::read_contained_opt(run_dir, p)?.ok_or_else(|| {
                        format!(
                            "resume: {ev} checkpoint for step '{step}' names missing artifact '{p}'"
                        )
                    })?,
                    None => String::new(),
                };
                // Fan-out steps route against the headerless plain
                // concatenation, NOT the labeled aggregate the chain file
                // holds: live routing matches `plain`, so a resume that
                // re-reads the chain would test conditions against "--- id ---"
                // headers and could pick a different branch.
                let plain = match v.get("plain_file").and_then(|x| x.as_str()) {
                    Some(p) => Some(contain::read_contained_opt(run_dir, p)?.ok_or_else(|| {
                        format!(
                            "resume: aggregate checkpoint for step '{step}' names missing artifact '{p}'"
                        )
                    })?),
                    None => None,
                };
                if let Some(expected) = v.get("output_hash").and_then(|x| x.as_str()) {
                    if ev == "aggregate_end" {
                        if let Some(restored) = plain.as_deref() {
                            if !output_fingerprint_matches(expected, restored) {
                                return Err(format!(
                                    "resume: {ev} checkpoint for step '{step}' does not match the recorded output hash"
                                ));
                            }
                        }
                    } else if !output_fingerprint_matches(expected, &chain) {
                        pending_step_hashes.insert((step.clone(), visit), expected.to_string());
                    }
                }
                let exit = exit_raw.and_then(|c| i32::try_from(c).ok()).unwrap_or(1);
                let timed_out = v
                    .get("timed_out")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                // Both event types, not just step_end. The LIVE path already
                // wraps a failed fan-out's text in the same banner, so leaving
                // aggregate_end out meant a resumed run handed downstream steps
                // a failed group's output as if it were a clean result - and a
                // forged aggregate_end could hand them any text in the run dir
                // with nothing marking it. Live and resumed have to agree.
                let exposed = if !ok {
                    failed_output(&step, chain.trim_end(), exit, timed_out)
                } else {
                    chain.trim_end().to_string()
                };
                // `outputs` is the pre-compact TEXT, never the raw tool
                // output: out_file holds a claude JSON envelope or a codex
                // event stream, and restoring that would inject machine
                // noise into a resumed prompt. Uncompacted steps have no
                // separate original, so chain is the original. A compacted
                // step patches this from its compact_end event below.
                // out_file gets the same canonicalized containment check as
                // chain_file: the old lexical-only test let a symlink inside
                // the run dir point at an arbitrary path (S1-4).
                let out_canon = match v.get("out_file").and_then(|x| x.as_str()) {
                    Some(p) => contain::contained_opt(run_dir, p)?,
                    None => None,
                };
                let file = out_canon
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                // The stderr path is derived from the (already contained) out_file,
                // but the .err.txt it names could itself be a symlink planted in the
                // run dir and pointing outside it; `{{steps.x.stderr_file}}` would
                // then hand a downstream agent/cmd a path that reads an external
                // file. Require the derived path to resolve under the run dir, not
                // merely to exist, before exposing it (rev_break #5).
                let stderr_file = out_canon
                    .as_ref()
                    .map(|p| stderr_file_for(p))
                    .filter(|p| p.exists() && contain::is_under(run_dir, p))
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                st.outputs.insert(
                    step.clone(),
                    template::StepOutput {
                        output: exposed.clone(),
                        outputs: exposed,
                        output_file: file,
                        exit,
                        stderr_file,
                        outcome: v
                            .get("outcome")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        label: v
                            .get("outcome_label")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    },
                );
                if !is_child {
                    if let Some(p) = v.get("chain_file").and_then(|x| x.as_str()) {
                        if let Some(canon) = contain::contained_opt(run_dir, p)? {
                            st.chain_files.insert(step.clone(), canon);
                        }
                    }
                    if ok {
                        st.last_success = Some(step.clone());
                    }
                    let replay_failed_control = !ok
                        && current_flow
                            .and_then(|f| f.steps.iter().find(|candidate| candidate.id == step))
                            .and_then(|candidate| candidate.on_error.as_deref())
                            .is_some_and(|action| action != "fail");
                    st.pending_route = if next_fallback.is_some() {
                        // This step_end closes one failed attempt but also
                        // atomically checkpoints the fallback that must run
                        // next. It is not a routable completion yet.
                        None
                    } else if ev == "step_end" && completed_event && (ok || replay_failed_control) {
                        Some(PendingRoute {
                            step: step.clone(),
                            visit,
                            route_text: chain.trim_end().to_string(),
                            errored: !ok,
                            from_plain: false,
                            members: None,
                            protocol: v
                                .get("protocol_state")
                                .and_then(|value| value.as_str())
                                .and_then(protocol::ProtocolState::parse),
                            postprocess: postprocess_pending,
                            compact_done: false,
                            notes_done: false,
                        })
                    } else if ev == "aggregate_end"
                        && completed_event
                        && (ok || replay_failed_control)
                    {
                        // Runs written before plain_file existed have no
                        // headerless copy: leave the route unset (as before)
                        // so the fan-out re-runs rather than routing on
                        // headered text live never matched against.
                        let members = restored_members(&v);
                        plain.map(|p| PendingRoute {
                            step: step.clone(),
                            visit,
                            route_text: p.trim_end().to_string(),
                            errored: !ok,
                            from_plain: true,
                            members,
                            protocol: None,
                            postprocess: postprocess_pending,
                            compact_done: false,
                            notes_done: false,
                        })
                    } else {
                        None
                    };
                    if let Some(profile) = &next_fallback {
                        unfinished.insert(
                            (step.clone(), visit),
                            UnfinishedStep {
                                step: step.clone(),
                                visit,
                                started: v
                                    .get("ts")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("unknown")
                                    .to_string(),
                                cmd: format!("fallback profile '{profile}'"),
                                profile: Some(profile.clone()),
                            },
                        );
                    }
                }
                // Trust model for the recorded session (rev_break #11): the
                // session id/marker/access restored here come from log.jsonl,
                // which is mutable on --resume. The access-escalation guard they
                // feed (leaf::prepare_leaf) is aimed at untrusted CONTENT a read
                // step ingested (e.g. a web page) being promoted into a write/full
                // agent - that attacker controls the content, not this run dir, so
                // the dir's own record is the trustworthy source. A LOCAL attacker
                // who can edit log.jsonl already has the user's OS privileges (the
                // runs root is 0700) and can equally edit the flow or the prompts;
                // the log is not a boundary against them. Two hardenings still
                // apply: a MISSING access fails closed (cannot be deleted to
                // escalate), and a resume that reports no session id is refused
                // (check_session), so a fresh/other session cannot be passed off
                // as the recorded one.
                if ok {
                    if let Some(s) = v.get("session") {
                        if let (Some(t), Some(id)) = (
                            s.get("tool").and_then(|x| x.as_str()),
                            s.get("id").and_then(|x| x.as_str()),
                        ) {
                            st.sessions.insert(
                                step.clone(),
                                leaf::SessionInfo {
                                    tool: t.to_string(),
                                    id: id.to_string(),
                                    cwd: s.get("cwd").and_then(|x| x.as_str()).map(String::from),
                                    marker: s
                                        .get("marker")
                                        .and_then(|x| x.as_str())
                                        .map(String::from),
                                    // Absent in logs written before access was
                                    // recorded: the resume guard warns on None.
                                    access: s
                                        .get("access")
                                        .and_then(|x| x.as_str())
                                        .and_then(|a| preset::Access::parse(Some(a)).ok()),
                                    // Present-but-unparsable stays `true`: the
                                    // key IS there, it is just not a level, and
                                    // that is an edit rather than an old run.
                                    access_recorded: s.get("access").is_some(),
                                },
                            );
                        }
                    }
                }
            }
            "notes_end" => {
                let visit = restored_visit(&v, &step)?;
                if let Some(pending) = st.pending_route.as_mut() {
                    if pending.step == step && pending.visit == visit {
                        let marker = v
                            .get("marker")
                            .and_then(|x| x.as_str())
                            .filter(|marker| !marker.is_empty())
                            .ok_or_else(|| {
                                format!("resume: notes checkpoint for step '{step}' has no marker")
                            })?;
                        let notes = contain::read_contained_opt(run_dir, "notes.md")?
                            .ok_or_else(|| {
                                format!(
                                    "resume: notes checkpoint for step '{step}' names missing artifact 'notes.md'"
                                )
                            })?;
                        if !notes.lines().any(|line| line.trim() == marker) {
                            return Err(format!(
                                "resume: notes checkpoint for step '{step}' is missing marker '{marker}'"
                            ));
                        }
                        pending.notes_done = true;
                    }
                }
            }
            "postprocess_end" => {
                let visit = restored_visit(&v, &step)?;
                if let Some(pending) = st.pending_route.as_mut() {
                    if pending.step == step && pending.visit == visit {
                        pending.postprocess = false;
                        pending.compact_done = true;
                        pending.notes_done = true;
                    }
                }
            }
            "position" => {
                st.pending_route = None;
                let next = v.get("next").and_then(|x| x.as_str()).unwrap_or("");
                if next == "end" || next == "fail" {
                    st.completed = true;
                    st.start = None;
                } else if next == "stuck" {
                    // A stuck run is NOT completed - the whole point is that a
                    // human looks at it and resumes. It resumes by re-running
                    // the step that made the decision, which is why the pending
                    // route is left cleared above: replaying the recorded
                    // verdict would evaluate the same text and route straight
                    // back to stuck, for ever. Re-running gives the step a new
                    // visit under the usual max_visits check.
                    //
                    // Reached through on_max_visits instead, that same restart
                    // walks back into the exhausted node and sticks again on
                    // entry. That is the honest answer: resetting the visit
                    // counter here would quietly undo the limit the flow set.
                    // The way out is to fix max_visits and --force-resume.
                    st.completed = false;
                    st.start = v.get("after").and_then(|x| x.as_str()).map(String::from);
                } else {
                    st.completed = false;
                    // Routing INTO a fan-out is the flow asking for a fresh lap
                    // of it, however the previous lap ended. The run did not
                    // simply stop at a failure, so there is nothing to carry.
                    carry_from.remove(next);
                    st.start = Some(next.to_string());
                }
            }
            // One landing per run, across resumes too. Only the fact is
            // restored, not the trigger or the numbers: what matters on the way
            // back in is that the wrap-up chain has already been paid for once.
            "budget_landing" => {
                st.budget_landed = true;
                st.retry_budget_trigger = None;
            }
            // The workspace baseline this run last committed to. Later entries
            // replace earlier ones - the newest durable checkpoint is the one a
            // resume compares against - and an adoption is a checkpoint too.
            "workspace_checkpoint" | "workspace_adopted" => {
                if let Some(fp) = v
                    .get("fingerprint")
                    .or_else(|| v.get("to"))
                    .and_then(|x| x.as_str())
                {
                    st.workspace_checkpoint = Some(fp.to_string());
                }
            }
            "run_end" => {
                run_end_elapsed = v.get("elapsed_sec").and_then(|x| x.as_u64());
            }
            _ => {}
        }
    }
    if let Some(((step, _), _)) = pending_step_hashes.into_iter().next() {
        return Err(format!(
            "resume: step_end checkpoint for step '{step}' does not match the recorded output hash"
        ));
    }
    st.member_fallbacks
        .retain(|(parent, visit, _), _| unfinished.contains_key(&(parent.clone(), *visit)));
    st.unfinished_step = unfinished.into_values().next_back();
    if st.pending_route.is_none() && !st.completed {
        if let Some(u) = &st.unfinished_step {
            st.start = Some(u.step.clone());
        } else if st.start.is_none() {
            // A failed step ended without recording its on_error decision.
            st.start = last_step;
        }
    }
    // A fan-out that FAILS still logs an aggregate_end, so `visits` restores
    // that visit and the resume re-enters the group one higher - while the
    // members that finished are recorded under the visit that crashed. Without
    // carrying them forward the lookup misses every one and the resume re-runs,
    // and re-bills, the whole batch. That is the F-2 double-billing bug: the
    // skip logic was right, the key it looked under was one lap stale.
    //
    // The condition is exactly: this resume restarts at the group, and the
    // group's most recent lap ended in FAILURE at the visit whose members we
    // are carrying. Nothing else qualifies, and the three narrower conditions
    // tried before this all leaked one of these cases:
    //
    //   killed with no aggregate_end -> no entry here, and the resume re-enters
    //     the SAME visit, which the original key already covers. Carrying
    //     forward as well planted a set that a later route-back picked up.
    //   lap SUCCEEDED, flow routed back, crash before the new lap finished ->
    //     failed=false, so no carry. The old "highest visit with completed
    //     members" test saw visit 1 and skipped the whole of visit 2, which the
    //     flow had deliberately asked for.
    //   two crashes running -> the second lap logs its own failed aggregate_end
    //     and its carried members are in the log as members_restored, so the
    //     third attempt carries from visit 2, not visit 1.
    if let Some(resume_at) = st.start.clone() {
        if let Some(&visit) = carry_from.get(&resume_at) {
            if let Some(set) = st
                .completed_members
                .get(&(resume_at.clone(), visit))
                .cloned()
            {
                let next_visit = visit.checked_add(1).ok_or_else(|| {
                    format!("resume: step '{resume_at}' exhausted the supported visit counter")
                })?;
                // extend, not or_insert: a member known complete from either
                // source is complete, and replacing an existing entry - or
                // declining to touch it - would drop one side of the union.
                st.completed_members
                    .entry((resume_at, next_visit))
                    .or_default()
                    .extend(set);
            }
        }
    }
    // ---- restore active time (P1-03) ----
    // Three possible sources, consulted in this order - most durable first,
    // and NEVER blended, because status.json's own elapsed already includes
    // whatever came before it and adding a second source on top would
    // double it:
    //
    // 1. `run_end_elapsed`, above: a run_end event, logged once at the true
    //    end of an attempt and never rewritten. Most trusted because it is
    //    both durable AND known-final.
    // 2. meta.json's `elapsed_sec`: refreshed at that same final moment
    //    (see the `meta_final` write in run_inner), so it normally agrees
    //    with (1) exactly - it only falls behind when the attempt that
    //    would have refreshed it was killed first, in which case it still
    //    holds the value from the START of that attempt: a valid, if
    //    stale, lower bound rather than the true total.
    // 3. status.json's `elapsed_sec`: refreshed on every heartbeat, so the
    //    freshest source for a run that crashed mid-attempt with neither of
    //    the above - but the least durable of the three, because
    //    detach_run deletes a resumed run's copy and seeds a zeroed
    //    replacement before the child even starts (see the comment on
    //    `carried_elapsed`, above, for the same durability gap encountered
    //    one layer up).
    //
    // Whichever of the three answers, `carried_elapsed` is still applied as
    // an unconditional FLOOR afterwards: what this run itself durably
    // inherited via --carry-budget-from cannot be un-inherited by an
    // earlier ancestor's record going quiet (see the `budget_carried`
    // handling above for why that value is a floor and not an addend here
    // too).
    let meta_elapsed_sec = contain::read_contained_opt(run_dir, "meta.json")?
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|meta| meta.get("elapsed_sec").and_then(|v| v.as_u64()));
    let status_elapsed_sec = contain::read_contained_opt(run_dir, "status.json")?
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|status| status.get("elapsed_sec").and_then(|v| v.as_u64()));
    st.elapsed_sec = run_end_elapsed
        .or(meta_elapsed_sec)
        .or(status_elapsed_sec)
        .unwrap_or(0)
        .max(carried_elapsed);
    Ok(st)
}

/// Make the session access levels restored from log.jsonl agree with what the
/// flow actually declares (rev_break #11). The log is mutable on --resume, so a
/// recorded access the flow cannot have produced - e.g. a read step's session
/// edited to "access":"full" - is tampering: it is dropped to None, which the
/// resume guard in leaf::prepare_leaf fails closed on. The flow itself is the
/// trustworthy source because check_flow_fingerprint verified it is unchanged
/// (callers skip this reconciliation when --force-resume waived that check).
///
/// A fallback profile can legitimately run a step at a different tier than its
/// primary settings, so the recorded level may be the primary access OR any
/// fallback profile's access; anything else is the tampered case.
///
/// `legacy_era` covers runs created by sfh 0.x, before access was recorded at
/// all: their logs have NO access field, which is honest, not tampered. For
/// them a missing level is filled from the flow's primary declaration - what
/// the step actually ran under - so pre-1.0 continue_from/fork_from still
/// resume at write/full without an override (rev_regression: old runs could
/// not be resumed at any tier above read once the guard failed closed).
fn reconcile_session_access(
    sessions: &mut HashMap<String, leaf::SessionInfo>,
    flow: &flow::Flow,
    legacy_era: bool,
) {
    for (step_id, info) in sessions.iter_mut() {
        let Some(step) = flow.find_step(step_id) else {
            continue;
        };
        let Ok(primary) = leaf::effective(flow, step) else {
            continue;
        };
        // Any level the step COULD have run at, primary or fallback, counts as
        // untampered. A reviewer noted that when a fallback declares a higher
        // level than the primary, a forged log claiming that higher level is
        // accepted - which is true, and deliberate. The set comes from the flow,
        // and check_flow_fingerprint has already established the flow is the one
        // that produced this run: the author declared that this step may run at
        // that level, so resuming its session there is inside what they asked
        // for. Narrowing it would need the log to say WHICH profile ran, and the
        // log is the thing being distrusted. Recorded as B-16.
        let mut possible: Vec<preset::Access> = vec![primary.access];
        for fb in &step.fallback {
            if let Ok(e) = leaf::effective_with(flow, step, Some(fb)) {
                possible.push(e.access);
            }
        }
        match info.access {
            Some(a) if !possible.contains(&a) => {
                eprintln!(
                    "sfh: warning: step '{step_id}' recorded access {} but the flow declares {}; treating the recorded level as tampered and failing closed on any escalation",
                    a.as_str(),
                    possible
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join("/")
                );
                info.access = None;
            }
            // A genuinely pre-1.0 run has NO access key, which is honest. A run
            // that has the key but whose value is not a level has been edited,
            // and claiming to be old must not launder that: without
            // access_recorded here, setting `sfh_version: 0.x` and corrupting
            // the level got the same free fill as an authentic old run.
            None if legacy_era && !info.access_recorded => {
                info.access = Some(primary.access);
            }
            _ => {}
        }
    }
}

/// Newest run directory produced by THIS flow file. Several flows usually share
/// one runs root, so picking the newest directory overall would resume the
/// wrong run.
///
/// Trust bound (rev_break #12): this only matches meta.flow and the presence of
/// log.jsonl, so in a SHARED, world-writable runs root an attacker could plant a
/// directory that matches and have --resume-latest pick it. The defence is that
/// the runs root sfh creates is 0700 (protect_runs_root); a caller who points
/// --runs-dir at a shared writable root is opting out of that guarantee. A
/// planted dir still has to survive the flow fingerprint check below (unless
/// --force-resume) and, once loaded, every artifact path it records is
/// canonicalized and required to stay inside it (load_resume / contained_opt),
/// so it cannot read or write outside itself - but the root itself must be
/// trusted for --resume-latest to be meaningful.
fn latest_run_dir(root: &Path, flow_path: &Path) -> Option<PathBuf> {
    let want = abs(flow_path).display().to_string();
    // A planted symlink/junction under the runs root must not be selected as
    // "the latest run": is_dir()/exists() follow links, so the old filter would
    // happily resume a directory OUTSIDE the root. Enumerate by lstat and
    // require the resolved candidate to stay under the resolved root, the same
    // rule watch::run_dirs applies to status/wait/stop (rev_break #7).
    let canon_root = root.canonicalize().ok()?;
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| {
            contain::read_contained_opt(p, "log.jsonl")
                .map(|log| log.is_some())
                .unwrap_or(false)
        })
        .filter(|p| match p.canonicalize() {
            Ok(c) => c.starts_with(&canon_root),
            Err(_) => false,
        })
        .filter(|p| {
            // A meta.json that is a symlink or resolves outside the candidate
            // is not a vote for that candidate: read it contained (rev_break #6).
            contain::read_contained_opt(p, "meta.json")
                .ok()
                .flatten()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|m| m.get("flow").and_then(|x| x.as_str()).map(String::from))
                .map(|f| f == want)
                .unwrap_or(false)
        })
        .collect();
    dirs.sort();
    dirs.pop()
}

/// Hand this run to a background copy of sfh, print the run dir, and return.
/// The child's command line is rebuilt from RunOpts rather than filtered out of
/// argv, so it does not depend on how the caller spelled the flags. The child
/// inherits `nonce` through SFH_NONCE so both processes record the same value.
fn detach_run(opts: &RunOpts, run_dir: &Path, is_resume: bool, nonce: &str) -> Result<i32, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate the sfh executable to detach: {e}"))?;

    let mut args: Vec<String> = vec!["run".into(), abs(&opts.flow_path).display().to_string()];
    for (k, v) in &opts.vars {
        args.push("--var".into());
        args.push(format!("{k}={v}"));
    }
    if let Some(e) = &opts.emit {
        args.push("--emit".into());
        args.push(e.clone());
    }
    if let Some(d) = &opts.runs_dir {
        args.push("--runs-dir".into());
        args.push(d.display().to_string());
    }
    // The background copy must resolve the same state root, the same overlays
    // and the same workspace this validation pass just resolved; otherwise the
    // detached run is a different run from the one that was checked.
    if let Some(d) = &opts.state_dir {
        args.push("--state-dir".into());
        args.push(d.display().to_string());
    }
    for p in &opts.profiles {
        args.push("--profiles".into());
        args.push(p.display().to_string());
    }
    if opts.adopt_workspace {
        args.push("--adopt-workspace".into());
    }
    // Absolute, like every other path here: the detached copy is started from
    // a directory of the harness's choosing, and a relative ancestor would
    // either miss or - worse - resolve to a different run.
    if let Some(d) = &opts.carry_budget_from {
        args.push("--carry-budget-from".into());
        args.push(abs(d).display().to_string());
    }
    if opts.no_partial_emit {
        args.push("--no-partial-emit".into());
    }
    if opts.verbose {
        args.push("--verbose".into());
    }
    if opts.quiet {
        args.push("--quiet".into());
    }
    // --resume-latest is pinned to a concrete directory here, so a run started
    // in between cannot become the child's target.
    if is_resume {
        args.push("--resume".into());
        args.push(run_dir.display().to_string());
        if opts.force_resume {
            args.push("--force-resume".into());
        }
    }
    args.push("--run-dir".into());
    args.push(run_dir.display().to_string());

    let status_path = run_dir.join("status.json");
    // A resumed run dir still holds the previous attempt's terminal status;
    // drop it so a stale "done" cannot be mistaken for this attempt's result.
    match std::fs::remove_file(&status_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "cannot remove the previous status {} before detach: {e}",
                status_path.display()
            ))
        }
    }

    let d = execute::spawn_detached(
        &exe,
        &args,
        &run_dir.join("detached.out.txt"),
        &run_dir.join("detached.err.txt"),
        &[("SFH_NONCE", nonce)],
    )?;
    if let Some(w) = &d.warning {
        eprintln!("sfh: warning: {w}");
    }
    // Bind the nonce to the child's pid AND start time BEFORE anything else
    // touches the run dir, so a `sfh stop` landing right after the detach
    // already sees a consistent (pid, start, nonce) triple. The child rewrites
    // the same bytes with its own pid - which is exactly d.pid - so there is no
    // window of disagreement (rev_break #8).
    let child_start = execute::pid_start_time(d.pid);
    if let Err(e) = contain::write_nonce(run_dir, d.pid, child_start, nonce) {
        let _ = execute::kill_pid_tree(d.pid);
        return Err(format!(
            "cannot write the stop nonce; killed detached pid {}: {e}",
            d.pid
        ));
    }
    // Seed status.json only if the child has not already written its own, so
    // `sfh status` has something to report either way and neither clobbers.
    // The create_new guard inside seed_status makes this race-free.
    if let Err(e) = seed_status(
        &status_path,
        &Status {
            state: "running",
            step: String::new(),
            started: utc_stamp(),
            // The seed knows nothing about steps yet; the child overwrites this
            // file with the real clocks as soon as it enters its first step.
            step_started: None,
            visit: 0,
            last_output: Arc::new(AtomicU64::new(0)),
            steps_done: 0,
            cost_usd: 0.0,
            elapsed_before_attempt: 0,
            attempt_started: Instant::now(),
            active_members: BTreeMap::new(),
            fanout_total: 0,
            fanout_completed: 0,
            run_dir: run_dir.display().to_string(),
            flow: abs(&opts.flow_path).display().to_string(),
            pid: d.pid,
            exit_code: None,
            emit_step: None,
            emit_file: None,
            error: None,
            error_code: None,
            unfinished_step: None,
            nonce: nonce.to_string(),
            pid_start: child_start,
        },
    ) {
        let _ = execute::kill_pid_tree(d.pid);
        return Err(e);
    }
    if opts.as_json {
        // A detached run has no result yet, so this envelope is explicitly
        // NON-terminal and hands back the handle plus the command that blocks
        // for the answer. `sfh status`/`wait`/`stop` all take the same run_dir,
        // so a caller can prove it is talking about this run and not the newest.
        machine::emit(&machine::envelope(
            "run",
            true,
            0,
            json!({
                "state": "running",
                "terminal": false,
                "detached": true,
                "pid": d.pid,
                "run_id": run_dir.file_name().map(|n| n.to_string_lossy().into_owned()),
                "run_dir": run_dir.display().to_string(),
                "flow": abs(&opts.flow_path).display().to_string(),
                "next_actions": next_actions_for("running", run_dir, &opts.flow_path, None),
            }),
        ));
        return Ok(0);
    }
    // stdout gets the run dir and nothing else, so a caller can capture it.
    println!("{}", run_dir.display());
    let _ = std::io::stdout().flush();
    if !opts.quiet {
        eprintln!(
            "sfh: detached (pid {}). poll with: sfh status {}",
            d.pid,
            execute::shell_quote(&run_dir.display().to_string())
        );
    }
    Ok(0)
}

/// Drop a self-ignoring .gitignore into the runs root, the way cargo does for
/// target/. Run dirs hold rendered prompts, model output and session ids in
/// plaintext inside the user's own repo; one `git add -A` should not be able
/// to publish them. A hostile repo can pre-place an EMPTY .gitignore to make
/// the old "skip if exists" behaviour leave everything tracked, so an existing
/// file is verified: unless an effective `*` pattern is present, sfh appends
/// one and says so.
fn protect_runs_root(root: &Path) -> Result<(), String> {
    contain::mkdir_private(root)
        .map_err(|e| format!("cannot create runs dir {}: {e}", root.display()))?;
    let f = root.join(".gitignore");
    const BODY: &str = "# Created by sfh. Run artifacts are not source.\n*\n";
    // Write AND re-read: a failure used to be silently ignored, so a
    // read-only .gitignore left every run artifact committable while the run
    // proceeded as if protected (S3-3).
    let write_and_verify = |text: &str| -> Result<(), String> {
        contain::write_private(&f, text).map_err(|e| {
            format!(
                "cannot write {} ({e}); run artifacts would be committable, so this run refuses to start",
                f.display()
            )
        })?;
        let back = std::fs::read_to_string(&f).map_err(|e| {
            format!(
                "cannot re-read {} after writing it ({e}); cannot confirm run artifacts are protected",
                f.display()
            )
        })?;
        if !gitignore_ignores_everything(&back) {
            return Err(format!(
                "{} was written but still does not ignore everything; run artifacts could be committed",
                f.display()
            ));
        }
        Ok(())
    };
    match std::fs::read_to_string(&f) {
        Err(_) => write_and_verify(BODY),
        Ok(text) => {
            if gitignore_ignores_everything(&text) {
                return Ok(());
            }
            eprintln!(
                "sfh: warning: {} does not ignore everything; appending '*' so run artifacts cannot be committed",
                f.display()
            );
            write_and_verify(&format!(
                "{text}\n# Added by sfh: run artifacts are not source.\n*\n"
            ))
        }
    }
}

/// True when the gitignore has an effective pattern that ignores every entry:
/// a non-comment line that is exactly `*` (or `/*`). A `*` inside a comment or
/// a pattern like `*.log` does NOT count - git would still track the rest.
fn gitignore_ignores_everything(text: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#') && (t == "*" || t == "/*")
    })
}

/// Change-detection fingerprint of the flow file, recorded in meta.json and
/// used to refuse `--resume` of a run whose flow has changed. SHA-256 because
/// this is a security boundary: with FNV-1a a crafted flow could collide with
/// another and slip a changed flow past the resume guard.
/// Line endings are normalised first. The same flow file checked out on
/// Windows and on Linux differs by a CR on every line, and hashing the raw
/// bytes made those two "different versions of the flow" - so a run dir moved
/// between machines, or a working copy re-checked-out under a different
/// core.autocrlf, could not be resumed even though nothing about the flow had
/// changed. Every other cross-OS decision in sfh is byte-identical; this one
/// has to be too. A CR that is NOT part of a line ending still changes the
/// hash, so this is not a way to smuggle an edit past the check.
fn fingerprint(s: &str) -> String {
    crate::sha256::hex(s.replace("\r\n", "\n").as_bytes())
}

/// Output checkpoints predate the SHA-256 migration used by current logs.
/// Verify both encodings rather than making old run directories unresumable;
/// an unknown or malformed encoding never counts as a match.
fn output_fingerprint_matches(recorded: &str, text: &str) -> bool {
    match recorded.len() {
        16 => legacy_fingerprint_fnv(text) == recorded,
        64 => fingerprint(text) == recorded,
        _ => false,
    }
}

/// The FNV-1a 64 fingerprint sfh <= 0.9 recorded in meta.json. Kept ONLY so
/// --resume can verify run dirs that predate the SHA-256 switch: such a dir
/// records no flow_fingerprint_algo, and comparing its 16-hex value against a
/// 64-hex SHA would report EVERY unchanged flow as "a different version",
/// making old runs unresumable. New runs always record sha256.
fn legacy_fingerprint_fnv(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// Recorded next to the fingerprint in meta.json so resume compares like with
/// like instead of guessing which algorithm wrote the stored value.
const FINGERPRINT_ALGO: &str = "sha256-nl";
/// What sfh 0.9 recorded: the same SHA-256, but over the raw bytes, so a
/// CRLF working copy and an LF one disagreed. Runs written by 0.9 are still
/// verified the way 0.9 computed it - re-hashing them the new way would
/// report every unchanged flow as changed.
const RAW_SHA_FINGERPRINT_ALGO: &str = "sha256";
/// meta.json dirs without a flow_fingerprint_algo field were written before
/// the field existed, when the algorithm was FNV-1a.
const LEGACY_FINGERPRINT_ALGO: &str = "fnv1a";

/// Verify that the flow file has not changed since the run dir was created,
/// honouring the algorithm the dir actually recorded (R-2). Returns Ok(())
/// when unchanged, Err when the flow differs or cannot be verified.
fn check_flow_fingerprint(
    meta: &serde_json::Value,
    flow_text: &str,
    dir: &Path,
    flow_path: &Path,
) -> Result<(), String> {
    let old_fp = meta
        .get("flow_fingerprint")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let old_algo = meta
        .get("flow_fingerprint_algo")
        .and_then(|x| x.as_str())
        .unwrap_or(LEGACY_FINGERPRINT_ALGO);
    // The older algorithms hashed raw bytes, so an old run and a re-checked-out
    // flow disagree over nothing but line endings - and those are exactly the
    // runs the legacy path exists to rescue. Accept the value computed either
    // way for them. This does not weaken anything: the check asks "is this the
    // same flow", and a flow that differs only in CRLF vs LF is the same flow,
    // which is why new runs normalise before hashing in the first place.
    // BOTH directions. Trying only the LF form covered an old run recorded on
    // Unix being resumed from a CRLF checkout, and left the reverse - recorded
    // on Windows, resumed from an LF checkout - rejected, which is the more
    // common way round for a project whose CI is Linux.
    let lf = flow_text.replace("\r\n", "\n");
    let crlf = lf.replace('\n', "\r\n");
    let first_match = |candidates: [String; 3]| -> String {
        candidates
            .iter()
            .find(|c| *c == old_fp)
            .cloned()
            .unwrap_or_else(|| candidates[0].clone())
    };
    let expected = match old_algo {
        FINGERPRINT_ALGO => fingerprint(flow_text),
        RAW_SHA_FINGERPRINT_ALGO => first_match([
            crate::sha256::hex(flow_text.as_bytes()),
            crate::sha256::hex(lf.as_bytes()),
            crate::sha256::hex(crlf.as_bytes()),
        ]),
        LEGACY_FINGERPRINT_ALGO => first_match([
            legacy_fingerprint_fnv(flow_text),
            legacy_fingerprint_fnv(&lf),
            legacy_fingerprint_fnv(&crlf),
        ]),
        other => {
            return Err(format!(
                "{} records an unknown flow_fingerprint_algo '{other}'; the flow cannot be verified as unchanged (use --force-resume to resume anyway)",
                dir.display()
            ))
        }
    };
    if old_fp != expected {
        return Err(format!(
            "{} was produced by a different version of {} (the flow file has changed since that run; use --force-resume to resume anyway)",
            dir.display(),
            flow_path.display()
        ));
    }
    Ok(())
}

fn check_effective_config_fingerprint(
    meta: &serde_json::Value,
    effective_config: &str,
    dir: &Path,
) -> Result<(), String> {
    let Some(old_fp) = meta
        .get("effective_config_fingerprint")
        .and_then(|x| x.as_str())
    else {
        // Runs written before 1.1.2 did not pin machine-level profiles. Keep
        // them resumable, but make the missing guarantee visible.
        eprintln!(
            "sfh: warning: {} predates effective-config pinning; global profile changes cannot be verified for this legacy run",
            dir.display()
        );
        return Ok(());
    };
    let algo = meta
        .get("effective_config_fingerprint_algo")
        .and_then(|x| x.as_str())
        .unwrap_or(FINGERPRINT_ALGO);
    if algo != FINGERPRINT_ALGO {
        return Err(format!(
            "{} records unknown effective_config_fingerprint_algo '{algo}' (use --force-resume to override)",
            dir.display()
        ));
    }
    let current = fingerprint(effective_config);
    if old_fp != current {
        return Err(format!(
            "{} resolves to a different effective configuration now (global profiles, tool/model/access/args/env/cwd or defaults changed; restore the original profiles or use --force-resume)",
            dir.display()
        ));
    }
    Ok(())
}

struct Status {
    state: &'static str,
    step: String,
    started: String,
    /// When the current step was entered, and which visit it is. Elapsed time
    /// for the WHOLE run says nothing about whether the step in front of you is
    /// moving; these two plus `last_output` are what tell a poller that.
    step_started: Option<String>,
    visit: u32,
    /// Unix-epoch seconds of the last output from ANY child of this run, or 0
    /// if none has spoken yet. Written by the reader threads (execute::Observe),
    /// read by the heartbeat, so a status write always carries a fresh value.
    last_output: Arc<AtomicU64>,
    steps_done: u32,
    cost_usd: f64,
    elapsed_before_attempt: u64,
    attempt_started: Instant,
    active_members: BTreeMap<String, String>,
    fanout_total: u32,
    fanout_completed: u32,
    run_dir: String,
    flow: String,
    /// Which process owns this run; `sfh status` checks it is still alive so a
    /// run killed along with its parent reads as dead, not as running.
    pid: u32,
    /// Set once the run reaches a terminal state, so `sfh wait` can hand the
    /// result back with the same exit code a foreground run would have used.
    exit_code: Option<i32>,
    emit_step: Option<String>,
    emit_file: Option<String>,
    error: Option<String>,
    /// Stable machine classification corresponding to `error`. Additive: the
    /// prose remains for humans and older readers.
    error_code: Option<String>,
    unfinished_step: Option<UnfinishedStep>,
    /// Random token proving this status.json was written by the sfh that owns
    /// the run dir. `sfh stop` refuses to kill without a matching nonce file.
    nonce: String,
    /// Start time of the owning process, so a reused pid is told apart from
    /// the process that started the run (rev_break #8).
    pid_start: Option<u64>,
}

fn status_json(s: &Status) -> Result<String, String> {
    let unfinished_step = s.unfinished_step.as_ref().map(|u| {
        json!({
            "step": u.step,
            "visit": u.visit,
            "started_utc": u.started,
            "cmd": u.cmd,
            "profile": u.profile,
            "will_rerun": true,
        })
    });
    let last_output = match s.last_output.load(std::sync::atomic::Ordering::Relaxed) {
        0 => serde_json::Value::Null,
        secs => json!(utc_stamp_at(secs)),
    };
    let cost = serde_json::Number::from_f64(s.cost_usd)
        .ok_or_else(|| "cannot serialize status: cost_usd is not finite".to_string())?;
    let v = json!({
        "schema_version": 1,
        "state": s.state,
        "current_step": s.step,
        "started_utc": s.started,
        "step_started_utc": s.step_started,
        "last_output_utc": last_output,
        "visit": s.visit,
        "heartbeat_utc": utc_stamp(),
        "steps_done": s.steps_done,
        "cost_usd": cost,
        "elapsed_sec": s.elapsed_before_attempt.saturating_add(s.attempt_started.elapsed().as_secs()),
        "active_members": s.active_members,
        "fanout_total": s.fanout_total,
        "fanout_completed": s.fanout_completed,
        "run_dir": s.run_dir,
        "flow": s.flow,
        "pid": s.pid,
        "sfh_version": VERSION,
        "exit_code": s.exit_code,
        "emit_step": s.emit_step,
        "emit_file": s.emit_file,
        "error": s.error,
        "error_code": s.error_code,
        "unfinished_step": unfinished_step,
        "nonce": s.nonce,
        "pid_start": s.pid_start,
    });
    serde_json::to_string_pretty(&v).map_err(|e| format!("cannot serialize status: {e}"))
}

/// Stamp a terminal state onto an existing run's status.json without going
/// through the live `Status` machinery.
///
/// Used by the replay policy, which refuses before the run's status thread even
/// exists. The previous document's fields are preserved so `runs show`, `wait`
/// and `status` still see the run's history; only the outcome is replaced.
fn mark_terminal_status(
    dir: &Path,
    state: &str,
    exit: i32,
    code: machine::ErrorCode,
    error: &str,
) -> Result<(), String> {
    let path = dir.join("status.json");
    let mut v: serde_json::Value = contain::read_contained_opt(dir, "status.json")?
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({"schema_version": 1}));
    let Some(map) = v.as_object_mut() else {
        return Err(format!("{}: status.json is not an object", dir.display()));
    };
    map.insert("state".into(), json!(state));
    map.insert("exit_code".into(), json!(exit));
    map.insert("error".into(), json!(error));
    map.insert("error_code".into(), json!(code.as_str()));
    map.insert("heartbeat_utc".into(), json!(utc_stamp()));
    let text =
        serde_json::to_string_pretty(&v).map_err(|e| format!("cannot serialize status: {e}"))?;
    contain::write_private_atomic(&path, &text)
        .map_err(|e| format!("cannot persist status {}: {e}", path.display()))
}

fn write_status(path: &Path, s: &Status) -> Result<(), String> {
    let text = status_json(s)?;
    // Write-then-rename: `sfh status` and `sfh wait` poll this file every few
    // seconds, and a plain write lets them read a half-written document. Rename
    // is atomic on both platforms, so a reader sees the old or the new file and
    // never a torn one. The tmp name carries the pid: the detaching parent and
    // its child both write status.json, and a SHARED tmp name let them clobber
    // each other's in-flight write (rev_regression: detach status race).
    contain::write_private_atomic(path, &text)
        .map_err(|e| format!("cannot persist status {}: {e}", path.display()))
}

/// Seed status.json for a detached run ONLY if the child has not already written
/// its own. `create_new` is the atomic guard: an `exists()` check followed by a
/// write is not exclusive, so a short flow's child could finish (writing `done`)
/// between the parent's check and its write, and the parent's `running` seed
/// would then clobber the terminal status, leaving a finished run stuck on
/// `running` forever (rev_regression: detach status race). create_new fails
/// harmlessly once the file exists.
fn seed_status(path: &Path, s: &Status) -> Result<(), String> {
    use std::io::Write as _;
    let text = status_json(s)?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => {
            f.write_all(text.as_bytes())
                .map_err(|e| format!("cannot seed status {}: {e}", path.display()))?;
            f.sync_data()
                .map_err(|e| format!("cannot sync status {}: {e}", path.display()))?;
            contain::restrict_file(&f);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(format!("cannot create status {}: {e}", path.display())),
    }
}

/// Proof, read straight from the log, that an attempt reached a genuine
/// stop. Returns `(saw_run_end, log_recovers_a_terminal_landing)`:
///
/// - `saw_run_end`: a durable `run_end` event exists anywhere in the log.
///   It is the very last thing `run` ever logs for an attempt, written
///   exactly once, only at a true stop, so its presence alone proves
///   finality - see `carry_source_is_final`, the only caller that treats it
///   that way.
/// - `log_recovers_a_terminal_landing`: the log's own LAST recorded routing
///   decision already lands on "end", "fail" or "stuck" - the only three
///   positions a live run does not step away from - or it logged a
///   `persistence_failure`, which ends the attempt immediately for the same
///   reason. This is weaker than `saw_run_end` on its own (the process that
///   reached it could in principle still be alive), so callers pair it with
///   independent proof the owning process is gone.
fn log_terminal_evidence(dir: &Path) -> Result<(bool, bool), String> {
    let mut run_end = false;
    let mut last_position_terminal = false;
    let mut persistence_failure = false;
    if let Some(text) = contain::read_contained_opt(dir, "log.jsonl")? {
        for line in text.lines() {
            // Same fail-safe skip load_resume_for_flow uses: a torn last line
            // from a kill mid-write is evidence of nothing and must not be
            // misread as proof of anything (fail-SAFE, not fail-open - see
            // the matching comment there).
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            match v.get("event").and_then(|x| x.as_str()).unwrap_or("") {
                "run_end" => run_end = true,
                "persistence_failure" => persistence_failure = true,
                "position" => {
                    let next = v.get("next").and_then(|x| x.as_str()).unwrap_or("");
                    last_position_terminal = matches!(next, "end" | "fail" | "stuck");
                }
                _ => {}
            }
        }
    }
    Ok((run_end, last_position_terminal || persistence_failure))
}

/// Whether `dir` is a run `--carry-budget-from` may trust as a source: the
/// finality check, factored out of the carry closure in `run_inner` so it
/// can also back the `carryable` field `next_actions_for` reports for a run
/// that just ended (see there) - both questions are "can this run's numbers
/// be trusted as final", asked at different times.
///
/// When status.json can be read, its resolved state decides this exactly as
/// it always has: refuse a run that is "running", or "wedged" (dead-looking
/// on disk while the SAME process that started it is still alive - see
/// execute::pid_start_time). pid_alive alone is not enough here because pid
/// reuse makes it advisory: a run SIGKILLed with `state: running` still on
/// disk would read as alive forever the moment the OS handed its pid to
/// something unrelated, and `sfh stop` would refuse to clear it, leaving no
/// way to carry at all. The recorded start time is what tells the original
/// process from a stranger wearing its pid (rev_break #8). Anything else
/// status.json resolves to - done, failed, stuck, a confirmed dead, a
/// deliberate stop - is trusted, same as before.
///
/// When status.json cannot be read at all - missing, corrupt, or caught
/// mid-rewrite - that USED TO say nothing either way and carry anyway
/// (P1-02: fail-open in exactly the case that matters, a source run still
/// appending to its log). Proof now has to come from the log and the
/// process table instead: a durable `run_end` settles it outright; short of
/// that, the owning process must be independently confirmed gone (via the
/// run dir's own sfh-nonce, since status.json is exactly what is
/// unavailable here) AND the log's own last word must already be a
/// terminal landing. Absent both, the carry is refused rather than
/// assumed.
fn carry_source_is_final(dir: &Path) -> Result<(), String> {
    if let Ok(snap) = watch::read(dir) {
        // `pid_alive` first, and not merely because a gone process has no start
        // time to compare: a ZOMBIE keeps its /proc entry, so the recorded start
        // time still matches long after the process is dead, and "wedged" stayed
        // true forever on any host that does not reap orphans. The carry was
        // then refused with a message naming `sfh wait` and `sfh stop` - neither
        // of which can clear a zombie - so the only documented route out of a
        // SIGKILLed run was closed. Liveness is the question being asked here;
        // the start time only tells the original owner from a stranger wearing
        // its pid (rev_break #8), and is worth asking about a live pid alone.
        let wedged = snap.state == "dead"
            && snap.pid_start.is_some()
            && execute::pid_alive(snap.pid)
            && execute::pid_start_time(snap.pid) == snap.pid_start;
        if snap.state == "running" || wedged {
            return Err(format!(
                "that run is still going, so its spend is not final yet. Wait for it (`sfh wait {}`), or stop it (`sfh stop {}`), then carry.",
                dir.display(), dir.display()
            ));
        }
        return Ok(());
    }
    let (run_end, log_terminal) = log_terminal_evidence(dir)?;
    if run_end {
        return Ok(());
    }
    if log_terminal && matches!(watch::owner_verifiably_dead(dir)?, Some(true)) {
        return Ok(());
    }
    Err(format!(
        "cannot confirm that run has finished - its status.json is missing or unreadable, and its log does not yet prove it reached a terminal state. Wait for it (`sfh wait {}`), or stop it (`sfh stop {}`), then carry.",
        dir.display(), dir.display()
    ))
}

fn inherited_run_handoff_matches(
    dir: &Path,
    inherited_nonce: Option<&str>,
) -> Result<bool, String> {
    let Some(expected) = inherited_nonce else {
        return Ok(false);
    };
    let Some(raw) = contain::read_contained_opt(dir, "sfh-nonce")? else {
        return Ok(false);
    };
    let contain::Nonce::Bound { pid, start, nonce } = contain::parse_nonce(raw.trim())? else {
        return Ok(false);
    };
    if pid != std::process::id() || nonce != expected {
        return Ok(false);
    }
    Ok(start
        .map(|recorded| execute::pid_start_time(pid) == Some(recorded))
        .unwrap_or(true))
}

fn acquire_run_lease(
    dir: &Path,
    is_resume: bool,
    inherited_nonce: Option<&str>,
) -> Result<contain::RunLease, String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let lease = loop {
        match contain::try_run_lease(dir) {
            Ok(lease) => break lease,
            Err(contain::RunLeaseError::Busy)
                if inherited_nonce.is_some() && Instant::now() < deadline =>
            {
                // A detached child starts while its parent still owns the
                // lease. The parent publishes the child's nonce before
                // returning and releasing its handle, so this bounded wait is
                // the handoff rather than a second ownership path.
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(contain::RunLeaseError::Busy) => {
                return Err(format!(
                    "{}: another process owns this run directory ({})",
                    machine::ErrorCode::RunBusy.as_str(),
                    dir.display()
                ))
            }
            Err(contain::RunLeaseError::Io(error)) => {
                return Err(format!(
                    "cannot claim run directory {}: {error}",
                    dir.display()
                ))
            }
        }
    };

    if inherited_run_handoff_matches(dir, inherited_nonce)? {
        return Ok(lease);
    }
    if inherited_nonce.is_some() {
        return Err(format!(
            "{}: detached run ownership for {} could not be verified",
            machine::ErrorCode::RunBusy.as_str(),
            dir.display()
        ));
    }

    if is_resume {
        match watch::owner_verifiably_dead(dir)? {
            Some(false) => {
                return Err(format!(
                    "{}: the recorded owner of {} is still alive; wait for it or stop it before resuming",
                    machine::ErrorCode::RunBusy.as_str(),
                    dir.display()
                ))
            }
            Some(true) => return Ok(lease),
            None => {}
        }
        // Pre-nonce runs have no owner record to inspect. Their status still
        // gives a fail-closed fallback: a matching live pid is enough to
        // refuse, never enough to prove that an unrelated process is safe to
        // take over.
        if let Ok(snapshot) = watch::read(dir) {
            let same_live_process = execute::pid_alive(snapshot.pid)
                && snapshot
                    .pid_start
                    .map(|recorded| execute::pid_start_time(snapshot.pid) == Some(recorded))
                    .unwrap_or(true);
            if same_live_process {
                return Err(format!(
                    "{}: the recorded owner of {} may still be alive; wait for it or stop it before resuming",
                    machine::ErrorCode::RunBusy.as_str(),
                    dir.display()
                ));
            }
        }
        return Ok(lease);
    }

    let has_existing_artifact = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot inspect run directory {}: {error}", dir.display()))?
        .filter_map(Result::ok)
        .any(|entry| entry.file_name() != std::ffi::OsStr::new(contain::RUN_LOCK));
    if has_existing_artifact {
        return Err(format!(
            "--run-dir {} is not empty; choose a new or empty path, or use --resume for an existing run",
            dir.display()
        ));
    }
    Ok(lease)
}

fn verify_required_versions(flow: &flow::Flow) -> Result<(), String> {
    let mut requirements: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for resolved in flow.resolved_tools() {
        let Some(requirement) = resolved.require_version else {
            continue;
        };
        let program = resolved
            .bin
            .unwrap_or_else(|| preset::default_program(&resolved.tool));
        requirements
            .entry((resolved.tool, program))
            .or_default()
            .insert(requirement);
    }
    for ((tool, program), requirements) in requirements {
        let version = preflight::probe_version_for_run(&tool, &program).map_err(|error| {
            format!(
                "{}: cannot verify {tool} ({program}) before starting: {error}",
                machine::ErrorCode::CapabilityUnavailable.as_str()
            )
        })?;
        let observed = version.ok_or_else(|| {
            format!(
                "{}: {tool} ({program}) produced no usable --version output, so its require_version declaration cannot be verified",
                machine::ErrorCode::CapabilityUnavailable.as_str()
            )
        })?;
        for requirement in requirements {
            match crate::version::satisfies(&requirement, &observed) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(format!(
                        "{}: {tool} ({program}) reports {observed:?}, which does not satisfy require_version: {requirement}",
                        machine::ErrorCode::CapabilityUnavailable.as_str()
                    ))
                }
                Err(error) => {
                    return Err(format!(
                        "{}: {tool} ({program}) cannot be checked against require_version: {requirement}: {error}",
                        machine::ErrorCode::CapabilityUnavailable.as_str()
                    ))
                }
            }
        }
    }
    Ok(())
}

fn run_inner(opts: &RunOpts) -> Result<i32, String> {
    // `--runs-dir` keeps meaning exactly what it always meant, and with neither
    // flag the runs root is still `.sfh/runs`: every flow, script and CI job
    // that predates v1.2 lands in the same place.
    let state_root = state::StateRoot::resolve(opts.state_dir.as_deref(), opts.runs_dir.as_deref());
    let runs_root = state_root.runs_dir();

    // Resolve the resume target BEFORE loading the flow. The era recorded in
    // meta.json decides whether the lenient loader (which restores pre-1.0
    // defaults for flows that predate the mandatory-access and string-cmd
    // rules) may be used: a fresh run, or a resume of a run created by sfh
    // >= 1.0, always goes through the strict loader (rev_break #14 - the old
    // code fell back to lenient on ANY strict failure whenever --resume was
    // given, so a crafted run dir could execute a flow that a fresh run
    // rejects, at write access, without the flow ever having changed).
    let resume_dir: Option<PathBuf> = if opts.dry_run {
        None
    } else {
        resume_target(opts, &runs_root)?.map(|d| abs(&d))
    };
    // meta.json is read through the containment check: a symlink at the fixed
    // name used to be followed to an external JSON file that was then ingested
    // as the run's recorded vars and fingerprint (rev_break #6). A missing
    // meta.json is still allowed (a very old run dir); a violation is fatal.
    let resume_meta: Option<serde_json::Value> = match &resume_dir {
        Some(dir) => Some(match contain::read_contained_opt(dir, "meta.json")? {
            Some(t) => serde_json::from_str(&t)
                .map_err(|e| format!("{}: unreadable meta.json: {e}", dir.display()))?,
            None => json!({}),
        }),
        None => None,
    };
    // Legacy era = created by sfh 0.x, before the mandatory-access rule and
    // the string-cmd template ban. Such a run's flow legitimately parsed
    // under the old rules, so the lenient loader restores them; the flow
    // fingerprint check below still refuses a flow that actually CHANGED
    // without --force-resume (rev_regression R-2).
    let legacy_era = resume_meta
        .as_ref()
        .and_then(|m| m.get("sfh_version").and_then(|x| x.as_str()))
        .map(|v| v.starts_with("0."))
        .unwrap_or(false);
    let flow = match flow::load_with_overlays(&opts.flow_path, &opts.profiles) {
        Ok(f) => f,
        Err(e) => {
            if resume_dir.is_some() && legacy_era && opts.profiles.is_empty() {
                flow::load_lenient(&opts.flow_path).map_err(|_| e)?
            } else {
                return Err(e);
            }
        }
    };
    if !opts.dry_run && !opts.quiet {
        for warning in flow::runtime_warnings(&flow) {
            eprintln!("sfh: warning: {warning}");
        }
    }
    let effective_config = flow.effective_config_json()?;
    let effective_config_fp = fingerprint(&effective_config);
    let mut vars = flow.vars_string_map()?;
    let step_ids = flow.step_ids();
    if let Some(e) = &opts.emit {
        if !step_ids.contains(e) {
            return Err(format!("--emit '{e}' is not a step id"));
        }
    }

    let flow_dir = abs(opts.flow_path.parent().unwrap_or(Path::new(".")));
    let flow_text = std::fs::read_to_string(&opts.flow_path).map_err(|e| {
        format!(
            "cannot re-read flow {} for its execution fingerprint: {e}",
            opts.flow_path.display()
        )
    })?;
    let flow_fp = fingerprint(&flow_text);
    // Same rule `sfh validate` enforces through flow::validate: only characters
    // that would affect the run dir PATH are forbidden (R-6).
    let name = flow.name.clone().unwrap_or_else(|| "flow".into());
    flow::validate_name(&name)?;

    // Keys whose values came from the resumed run dir's meta.json (and were NOT
    // overridden by an explicit --var on THIS command). These are run-derived
    // UNTRUSTED input and may not flow into executed-privileged template sinks
    // (bin / cwd / argv[0]); an explicit --var is the user's own value and stays
    // trusted (rev_break #12).
    let mut tainted_vars: HashSet<String> = HashSet::new();
    if let Some(dir) = &resume_dir {
        let meta = resume_meta.as_ref().expect("resume meta read above");
        if !opts.force_resume {
            check_flow_fingerprint(meta, &flow_text, dir, &opts.flow_path)?;
            check_effective_config_fingerprint(meta, &effective_config, dir)?;
        }
        // A resumed dir must be a real directory, not a symlink/junction sfh
        // would resolve into somewhere the caller did not point at; and on Unix
        // a group/world-writable dir means another local user can swap files
        // between sfh's containment check and its open, so warn loudly about the
        // residual TOCTOU (rev_break #5).
        match dir.symlink_metadata() {
            Ok(md) if md.is_symlink() => {
                return Err(format!(
                    "{} is a symlink, not a run directory; refusing to resume through it",
                    dir.display()
                ))
            }
            #[cfg(unix)]
            Ok(md) => {
                use std::os::unix::fs::PermissionsExt;
                if md.permissions().mode() & 0o022 != 0 {
                    eprintln!(
                        "sfh: warning: {} is group/world-writable; another local user could modify it while this run reads it",
                        dir.display()
                    );
                }
            }
            _ => {}
        }
        // Recorded vars override the flow's defaults; an explicit --var on
        // THIS command overrides both (applied after this block).
        if let Some(obj) = meta.get("vars").and_then(|x| x.as_object()) {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    vars.insert(k.clone(), s.to_string());
                    tainted_vars.insert(k.clone());
                }
            }
        }
    }
    for (k, v) in &opts.vars {
        vars.insert(k.clone(), v.clone());
        tainted_vars.remove(k); // an explicit --var is the user's own value
    }
    precheck(&flow, &vars, &tainted_vars)?;
    // `plan` has a strict spawn-zero contract and only reports declarations.
    // A real run is already authorized to launch these programs; check their
    // inert --version path before creating/mutating a run dir or starting any
    // model process. Detached parents repeat this in the child after handoff,
    // closing the update race between approval and execution.
    if !opts.dry_run && resume_dir.is_none() {
        verify_required_versions(&flow)?;
    }

    // Which steps must produce resumable sessions (continue_from targets).
    let mut needed_sessions: HashSet<String> = HashSet::new();
    {
        let mut note = |s: &flow::Step| {
            for t in [&s.continue_from, &s.fork_from].into_iter().flatten() {
                needed_sessions.insert(t.clone());
            }
        };
        for s in &flow.steps {
            note(s);
            if let Some(children) = &s.parallel {
                for c in children {
                    note(c);
                }
            }
        }
    }

    // ---- pick or create the run directory ----
    let mut resumed = ResumeState::default();
    let run_dir: PathBuf;
    let mut is_resume = false;
    let inherited_nonce = std::env::var("SFH_NONCE")
        .ok()
        .filter(|nonce| !nonce.trim().is_empty());
    let mut _run_lease: Option<contain::RunLease>;
    if opts.dry_run {
        run_dir = std::env::temp_dir().join(format!("sfh-plan-{}", leaf::gen_uuid()));
        contain::mkdir_private(&run_dir).map_err(|e| {
            format!(
                "cannot create temporary plan dir {}: {e}",
                run_dir.display()
            )
        })?;
        _run_lease = None;
    } else if let Some(dir) = resume_dir {
        // The resumed dir was protected when it was first created; nothing
        // here writes to the runs root, so its state is not this run's
        // concern (and the root may be absent or read-only by design).
        _run_lease = Some(acquire_run_lease(&dir, true, inherited_nonce.as_deref())?);
        // Ownership comes first on resume: a duplicate resume must not get to
        // execute even a declared binary's --version while the live owner is
        // still working. This remains before log/status/workspace mutation and
        // before any flow step.
        verify_required_versions(&flow)?;
        resumed = load_resume_for_flow(&dir, Some(&flow))?;
        // Cross-check the recorded session access against the flow (which the
        // fingerprint check above verified is unchanged, unless --force-resume
        // waived it): a log edited to claim a higher access than the flow
        // declares for that step is tampering and reads back as None, which
        // the resume guard fails closed on (rev_break #11). A LEGACY-era run
        // predates access recording entirely, so a missing access there is
        // filled from the flow's own declaration instead - that is what the
        // step actually ran under, and it keeps pre-1.0 runs resumable at
        // write/full without an override (rev_regression: old continue_from).
        if opts.force_resume {
            // --force-resume waives the FINGERPRINT check, which is exactly the
            // thing that made the flow a trustworthy yardstick for the recorded
            // access. Skipping the cross-check here left the log's own claim as
            // the only evidence, so editing a read session to "access":"full"
            // and adding --force-resume walked straight past the escalation
            // guard. It is not a substitute for allow_access_override.
            //
            // Drop every restored level to unknown instead. The guard in
            // prepare_leaf fails closed on that, and a caller who really means
            // to resume these sessions says so per step with
            // allow_access_override: true.
            for info in resumed.sessions.values_mut() {
                info.access = None;
                info.access_recorded = true;
            }
        } else {
            reconcile_session_access(&mut resumed.sessions, &flow, legacy_era);
        }
        if resumed.completed {
            return Err(format!(
                "{} already completed - nothing to resume",
                dir.display()
            ));
        }
        if resumed.start.is_none() && resumed.pending_route.is_none() {
            return Err(format!(
                "{}: cannot tell where to resume from",
                dir.display()
            ));
        }
        if let Some(u) = &resumed.unfinished_step {
            // The replay decision. A step that started and never recorded an
            // end may have already done whatever it does out in the world, and
            // sfh cannot look and find out. Re-running is right for a pure
            // computation and wrong for a deploy, so the flow says which - and
            // the refusals happen HERE, before anything is launched.
            let policy = flow
                .find_step(&u.step)
                .map(|s| s.replay_policy(&flow))
                .unwrap_or_default();
            let effects = flow
                .find_step(&u.step)
                .map(|s| s.effects(&flow).as_str())
                .unwrap_or("unknown");
            match policy {
                flow::ReplayPolicy::Rerun => {
                    if let Some(profile) = &u.profile {
                        eprintln!(
                            "sfh: step '{}' selected fallback '{}' in visit {} but never recorded its end.\n     resuming that fallback directly: {}",
                            u.step, profile, u.visit, u.cmd
                        );
                    } else {
                        eprintln!(
                            "sfh: step '{}' started {} and never recorded an end.\n     resuming will run it again: {}",
                            u.step, u.started, u.cmd
                        );
                    }
                }
                flow::ReplayPolicy::Stuck | flow::ReplayPolicy::Fail => {
                    let (code, word) = match policy {
                        flow::ReplayPolicy::Stuck => (4, "stuck"),
                        _ => (1, "failed"),
                    };
                    let why = format!(
                        "step '{}' started {} and never recorded an end. It declares effects: {effects} with replay.unfinished: {}, so sfh will not launch it again without knowing whether its effects already happened.",
                        u.step,
                        u.started,
                        policy.as_str()
                    );
                    // Nothing is spawned, and the workspace and every partial
                    // artifact stay exactly where they are for a human to look
                    // at. This is a decision handed back, not a failure.
                    let mut log = contain::append_private(&dir.join("log.jsonl"))
                        .map_err(|e| format!("cannot open log: {e}"))?;
                    // Nothing ran this attempt, so the total active time is
                    // exactly what was already restored - carried forward
                    // rather than recomputed, for the same reason the other
                    // three run_end sites now record it durably (P1-03):
                    // this is the run's own strongest remaining source once
                    // status.json goes missing or stale.
                    log_event(
                        &mut log,
                        json!({"ts": utc_stamp(), "event": "run_end", "state": word,
                               "exit": code, "error": why.clone(),
                               "replay_refused": policy.as_str(), "step": u.step,
                               "elapsed_sec": resumed.elapsed_sec}),
                    )?;
                    mark_terminal_status(
                        &dir,
                        word,
                        code,
                        machine::ErrorCode::ReplayRefused,
                        &why,
                    )?;
                    if opts.as_json {
                        machine::emit(&machine::error_envelope(
                            "run",
                            machine::ErrorCode::ReplayRefused,
                            &why,
                            code,
                            json!({
                                "run_dir": dir.display().to_string(),
                                "state": word,
                                "terminal": true,
                                "step": u.step,
                                "effects": effects,
                                "replay_unfinished": policy.as_str(),
                            }),
                        ));
                    } else {
                        eprintln!("sfh: {why}");
                        eprintln!(
                            "sfh: the workspace and partial artifacts are preserved in {}",
                            dir.display()
                        );
                    }
                    return Ok(code);
                }
            }
        }
        run_dir = dir;
        is_resume = true;
    } else if let Some(d) = &opts.run_dir {
        // The detaching parent already picked (and created) this directory so
        // it could print the path before the background copy came up.
        let d = abs(d);
        contain::mkdir_private(&d)
            .map_err(|e| format!("cannot create run dir {}: {e}", d.display()))?;
        run_dir = d;
        _run_lease = Some(acquire_run_lease(
            &run_dir,
            false,
            inherited_nonce.as_deref(),
        )?);
    } else {
        protect_runs_root(&runs_root)?;
        match state_root.run_retention() {
            Ok(Some(policy)) => {
                let report = crate::runs::apply_retention(&runs_root, policy);
                if !report.removed.is_empty() && !opts.quiet {
                    let noun = if report.removed.len() == 1 {
                        "directory"
                    } else {
                        "directories"
                    };
                    eprintln!(
                        "sfh: retention removed {} old run {noun}",
                        report.removed.len(),
                    );
                }
                for warning in report.warnings {
                    eprintln!("sfh: warning: retention: {warning}");
                }
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("sfh: warning: retention disabled: {error}");
            }
        }
        let base = format!("{}-{}", utc_stamp(), name);
        let mut d = runs_root.join(&base);
        let mut n = 1;
        loop {
            match contain::mkdir_private_new(&d) {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    n += 1;
                    d = runs_root.join(format!("{base}-{n}"));
                }
                Err(error) => {
                    return Err(format!("cannot create run dir {}: {error}", d.display()))
                }
            }
        }
        run_dir = abs(&d);
        _run_lease = Some(acquire_run_lease(&run_dir, false, None)?);
    }
    // ---- inherit an earlier run's spend into this fresh one ----
    // Resolved AFTER the run dir exists, because the ancestor has to be told
    // apart from the run being started, and with no flow to check against: the
    // ancestor ran a different flow, which is the premise of the whole feature.
    let carry = (|| -> Result<Option<CarriedBudget>, String> {
        let dir =
            match &opts.carry_budget_from {
                None => return Ok(None),
                Some(_) if is_resume => return Err(
                    "--carry-budget-from starts a NEW run that inherits an earlier run's spend; \
                     --resume continues the earlier run itself. They answer different questions, \
                     so pick one: resume when the flow is unchanged, carry when you had to fix it."
                        .into(),
                ),
                Some(dir) => abs(dir),
            };
        if dir == run_dir {
            return Err("--carry-budget-from cannot name the run it is starting".into());
        }
        // A run that is still spending has no final total. Carrying from it
        // would take a snapshot the ancestor immediately invalidates, and the
        // numbers in both runs would then be wrong in a way nothing downstream
        // could detect. See carry_source_is_final for exactly what "final"
        // means and is proven from - in particular, an unreadable or absent
        // status.json no longer carries anyway (P1-02): it used to say
        // nothing either way and let the carry through regardless.
        carry_source_is_final(&dir)
            .map_err(|e| format!("--carry-budget-from {}: {e}", dir.display()))?;
        let prior = load_resume_for_flow(&dir, None)
            .map_err(|e| format!("--carry-budget-from {}: {e}", dir.display()))?;
        // A corrected flow may have renamed, split or removed steps. Laps
        // are only enforceable against an id that still exists, so the two
        // groups are separated and both are reported.
        let (visits, mut dropped): (BTreeMap<String, u32>, Vec<String>) =
            prior.visits.into_iter().fold(
                (BTreeMap::new(), Vec::new()),
                |(mut keep, mut drop), (step, visit)| {
                    if flow.find_step(&step).is_some() {
                        keep.insert(step, visit);
                    } else {
                        drop.push(step);
                    }
                    (keep, drop)
                },
            );
        // `prior.visits` is a HashMap, so without this the dropped list -
        // and therefore the message and the durable record - would come out
        // in a different order on every run.
        dropped.sort_unstable();
        Ok(Some(CarriedBudget {
            from: dir,
            visits,
            dropped,
            steps: prior.total,
            cost_usd: prior.cost_usd,
            elapsed_sec: prior.elapsed_sec,
        }))
    })();
    let carried = match carry {
        Ok(c) => c,
        Err(e) => {
            // The run dir was created just above, and a carry that never got
            // off the ground must not leave an empty one behind for `runs
            // list`, `runs clean` and the next run's name counter to trip
            // over. remove_dir, never remove_dir_all: nothing has been written
            // into it yet, so a non-recursive delete does the whole job and
            // can never grow into deleting a real run. A resumed dir and a
            // --run-dir handed in by the detaching parent are not ours to
            // remove.
            //
            // The one thing the directory does hold by now is this attempt's
            // own lease file, so the lease is dropped (releasing the OS lock,
            // and on Windows closing the handle that would otherwise refuse
            // the delete outright) and that one named file is removed first.
            // Removing only the lock keeps the safety property intact: any
            // OTHER entry still makes remove_dir fail and leaves the directory
            // exactly where it is.
            if !is_resume && opts.run_dir.is_none() {
                _run_lease = None;
                let _ = std::fs::remove_file(run_dir.join(contain::RUN_LOCK));
                let _ = std::fs::remove_dir(&run_dir);
            }
            return Err(e);
        }
    };
    if let Some(c) = &carried {
        // Counters only. Nothing that would make this look like a resume:
        // no outputs, no sessions, no routing position, no workspace.
        resumed.total = c.steps;
        resumed.cost_usd = c.cost_usd;
        resumed.elapsed_sec = c.elapsed_sec;
        resumed.visits = c.visits.iter().map(|(k, v)| (k.clone(), *v)).collect();
        // Said here rather than next to the durable event, so a `--dry-run`
        // that never opens a log still tells the caller what this run would
        // start with. Silence would make the flag look like a no-op.
        if !opts.quiet && !opts.as_json {
            eprintln!("sfh: {}", c.summary());
        }
    }
    // Defense-in-depth: even though `name` is charset-validated, confirm the
    // resolved run dir is actually under the runs root (guards symlink tricks).
    if !is_resume
        && !opts.dry_run
        && opts.run_dir.is_none()
        && !contain::is_under(&runs_root, &run_dir)
    {
        return Err(format!(
            "run dir {} escapes the runs root {}",
            run_dir.display(),
            runs_root.display()
        ));
    }
    // The nonce is minted in exactly ONE place per attempt. A detached child
    // inherits the parent's value through SFH_NONCE instead of minting its own,
    // so status.json and sfh-nonce can never disagree, even for the instant the
    // old "both sides generate" design left open (R-4). The nonce file binds
    // the token to the owning pid AND its start time (see contain::write_nonce),
    // which is what lets `sfh stop` refuse a status.json rewritten to point at
    // another pid, or a pid the OS reused after the run died (rev_break #8).
    let nonce = match std::env::var("SFH_NONCE") {
        Ok(n) if !n.trim().is_empty() => {
            // Consume it: a flow step that launches sfh itself must not carry
            // this run's nonce into the nested run.
            std::env::remove_var("SFH_NONCE");
            n.trim().to_string()
        }
        _ => contain::random_nonce(),
    };
    let pid_start = execute::pid_start_time(std::process::id());
    if !opts.detach && !opts.dry_run {
        // The detaching parent writes the file itself, after the spawn, when
        // it knows the child's pid (in detach_run).
        contain::write_nonce(&run_dir, std::process::id(), pid_start, &nonce)
            .map_err(|e| format!("cannot write the stop nonce: {e}"))?;
    }
    let notes_file = run_dir.join("notes.md");

    // ---- workspace ----
    // Decided from the flow's static shape (see Flow::workspace_plan), which is
    // why a plan and a run always agree about it. A flow with no `workspace:`
    // key resolves to `current` and creates nothing at all.
    let workspace_plan = flow.workspace_plan()?;
    for w in &workspace_plan.warnings {
        if !opts.quiet && !opts.as_json {
            eprintln!("sfh: warning: {w}");
        }
    }

    if opts.dry_run {
        let result = dry_run(
            &flow,
            &vars,
            &tainted_vars,
            &run_dir,
            &flow_dir,
            &notes_file,
            &needed_sessions,
            DryRunExtras {
                flow_path: &opts.flow_path,
                workspace: &workspace_plan,
                state_root: &state_root,
                profiles: &opts.profiles,
                as_json: opts.as_json,
                save: opts.save_plan.clone(),
            },
        );
        if let Err(e) = std::fs::remove_dir_all(&run_dir) {
            eprintln!(
                "sfh: warning: could not remove temporary plan dir {}: {e}",
                run_dir.display()
            );
        }
        return result;
    }

    // ---- hand the run off to a detached copy of ourselves ----
    // Everything above this point is validation, so a broken flow still fails
    // in the caller's face instead of dying silently in the background.
    if opts.detach {
        return detach_run(opts, &run_dir, is_resume, &nonce);
    }

    let elapsed_before_attempt = resumed.elapsed_sec;
    let mut log = contain::append_private(&run_dir.join("log.jsonl"))
        .map_err(|e| format!("cannot open log: {e}"))?;

    // Provenance: which sfh, which tool builds. Cheap, no AI calls. Probe only
    // the (tool, bin) pairs the flow actually resolves to: an unused profile's
    // bin is data, and data must never be executed.
    let mut tool_versions = serde_json::Map::new();
    if !is_resume {
        let mut by_tool: BTreeMap<String, BTreeSet<Option<String>>> = BTreeMap::new();
        for rt in flow.resolved_tools() {
            by_tool.entry(rt.tool).or_default().insert(rt.bin);
        }
        for (tool, bins) in by_tool {
            let entries: Vec<serde_json::Value> = bins
                .into_iter()
                .map(|bin| {
                    let program = bin.unwrap_or_else(|| preset::default_program(&tool));
                    let version = execute::probe_version(&program);
                    json!({"bin": program, "version": version})
                })
                .collect();
            if entries.len() == 1 {
                tool_versions.insert(tool, entries.into_iter().next().unwrap());
            } else {
                tool_versions.insert(tool, json!(entries));
            }
        }
    }
    // ---- execution closure ----
    // The flow fingerprint has always caught an edited flow. It cannot catch a
    // profile overlay that swapped the model, a context file that was rewritten
    // between the crash and the resume, or a CLI that upgraded itself - each of
    // which changes what the run does while leaving the flow byte-identical.
    let closure_now = build_closure(
        &flow,
        &opts.flow_path,
        &opts.profiles,
        &workspace_plan,
        (!is_resume).then_some(&tool_versions),
    )?;
    if is_resume {
        let recorded = contain::read_contained_opt(&run_dir, closure::FILE)?
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .as_ref()
            .and_then(closure::Closure::from_json);
        if let Some(before) = recorded {
            // The versions probed at the original run are part of the closure,
            // and a resume does not re-probe (that would be a second cost on a
            // path that is meant to be cheap), so compare on the entries this
            // attempt could actually reconstruct.
            let mut now = closure_now.clone();
            if let Some(tools) = before.get("tools") {
                now.set("tools", tools.clone());
            }
            let changes = before.diff(&now);
            if !changes.is_empty() {
                if opts.force_resume {
                    log_event(
                        &mut log,
                        json!({"ts": utc_stamp(), "event": "force_resume",
                               "reason": "execution_closure_changed", "changes": changes}),
                    )?;
                    if !opts.quiet && !opts.as_json {
                        eprintln!(
                            "sfh: warning: --force-resume accepted a changed execution closure:"
                        );
                        for c in &changes {
                            eprintln!("sfh:   {c}");
                        }
                    }
                } else {
                    return Err(format!(
                        "{}: the execution closure changed since this run started, so resuming would continue a different piece of work:\n  {}\nRe-run from scratch, restore the inputs, or pass --force-resume to accept the change.",
                        machine::ErrorCode::ExecutionClosureChanged.as_str(),
                        changes.join("\n  ")
                    ));
                }
            }
        }
    } else {
        let text = serde_json::to_string_pretty(&closure_now.to_json())
            .map_err(|e| format!("cannot serialize the execution closure: {e}"))?;
        contain::write_private_atomic(&run_dir.join(closure::FILE), text)
            .map_err(|e| format!("cannot persist the execution closure: {e}"))?;
        log_event(
            &mut log,
            json!({"ts": utc_stamp(), "event": "execution_closure",
                   "fingerprint": closure_now.fingerprint(), "algo": closure::ALGO}),
        )?;
    }

    // ---- managed workspace ----
    // At most ONE per run, whatever the step or visit count: analyze, implement,
    // test, review and every loop revisit share it, because they are all working
    // on the same thing and a per-step worktree would hide each step's changes
    // from the next.
    let mut workspace: Option<workspace::Workspace> = None;
    if is_resume {
        if let Some(ws) = contain::read_contained_opt(&run_dir, workspace::MANIFEST)?
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .as_ref()
            .and_then(workspace::Workspace::from_manifest)
        {
            let checkpoint = resumed.workspace_checkpoint.clone();
            let unfinished = resumed.unfinished_step.is_some();
            if workspace_plan.verify_on_resume {
                match workspace::detect_drift(&ws, checkpoint.as_deref(), unfinished) {
                    workspace::Drift::None => {}
                    workspace::Drift::Missing => {
                        return Err(format!(
                            "{}: the managed workspace {} no longer exists, so this run cannot continue where it left off",
                            machine::ErrorCode::WorkspaceMissing.as_str(),
                            ws.path.display()
                        ))
                    }
                    workspace::Drift::Unknown(why) => {
                        return Err(format!(
                            "{}: sfh cannot fingerprint the workspace {} ({why}), so it cannot tell whether anything changed",
                            machine::ErrorCode::WorkspaceDrift.as_str(),
                            ws.path.display()
                        ))
                    }
                    workspace::Drift::Changed { unfinished } => {
                        if opts.adopt_workspace {
                            let now = workspace::fingerprint(&ws.path)?;
                            log_event(
                                &mut log,
                                json!({"ts": utc_stamp(), "event": "workspace_adopted",
                                       "workspace_id": ws.id, "path": ws.path.display().to_string(),
                                       "from": checkpoint, "to": now}),
                            )?;
                            if !opts.quiet && !opts.as_json {
                                eprintln!(
                                    "sfh: --adopt-workspace accepted the current contents of {} as the new baseline",
                                    ws.path.display()
                                );
                            }
                        } else if unfinished {
                            // A step was in flight when the run stopped, so the
                            // difference is explainable. The replay policy - not
                            // this check - decides what happens to that step.
                            if !opts.quiet && !opts.as_json {
                                eprintln!(
                                    "sfh: warning: {} differs from its last checkpoint, which is consistent with the step that never finished",
                                    ws.path.display()
                                );
                            }
                        } else {
                            return Err(format!(
                                "{}: the managed workspace {} changed since this run's last checkpoint, and no step was in flight to explain it. Something outside this run edited it.\nPass --adopt-workspace to accept the current contents as the new baseline. (--force-resume is a different question: it waives the flow/closure check, not this one.)",
                                machine::ErrorCode::WorkspaceDrift.as_str(),
                                ws.path.display()
                            ));
                        }
                    }
                }
            }
            workspace = Some(ws);
        }
    } else if workspace_plan.resolved == flow::WorkspaceMode::GitWorktree {
        // With no `root:`, the workspace is a worktree of the repository the
        // CALLER is standing in - not of the repository the flow file happens
        // to live in. A managed workspace replaces the caller's cwd, which is
        // where every step ran before v1.2, so that is the directory it has to
        // be about. It also makes a shared flow work: `sfh run
        // /somewhere/else/flow.yaml` from your project operates on YOUR
        // project. An explicit `root:` is resolved against the flow file, like
        // every other path a flow declares.
        let source = match &workspace_plan.root {
            Some(r) => flow_dir.join(r),
            None => std::env::current_dir()
                .map_err(|e| format!("cannot resolve the current directory: {e}"))?,
        };
        let ws = workspace::create_git_worktree(
            &source,
            &state_root,
            &name,
            run_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| name.clone())
                .as_str(),
            workspace_plan.base.as_deref(),
        )?;
        log_event(
            &mut log,
            json!({"ts": utc_stamp(), "event": "workspace_created",
                   "workspace_id": ws.id, "mode": ws.mode.as_str(),
                   "path": ws.path.display().to_string(), "branch": ws.branch,
                   "base_ref": ws.base_ref, "base_commit": ws.base_commit}),
        )?;
        if !opts.quiet && !opts.as_json {
            eprintln!(
                "sfh: workspace: {} (branch {})",
                ws.path.display(),
                ws.branch.as_deref().unwrap_or("-")
            );
        }
        workspace = Some(ws);
    } else if workspace_plan.resolved == flow::WorkspaceMode::Directory {
        let root = workspace_plan
            .root
            .as_ref()
            .expect("workspace_plan rejects directory mode without a root");
        let path = flow_dir.join(root);
        if !path.is_dir() {
            return Err(format!(
                "workspace.root {} is not a directory. sfh does not create a `directory` workspace - it is yours, and creating or removing it is not sfh's to do.",
                path.display()
            ));
        }
        workspace = Some(workspace::Workspace {
            id: "primary".into(),
            mode: flow::WorkspaceMode::Directory,
            source_root: path.clone(),
            path,
            base_ref: None,
            base_commit: None,
            branch: None,
            // NOT sfh's: no marker, no nonce, and therefore never removable.
            created_by_sfh: false,
            ownership_nonce: None,
            cleanup: flow::WorkspaceCleanup::Keep,
        });
    }
    let workspace_path: Option<PathBuf> = workspace.as_ref().map(|w| w.path.clone());
    if let Some(ws) = &workspace {
        if !is_resume {
            let initial = (ws.mode == flow::WorkspaceMode::GitWorktree)
                .then(|| workspace::fingerprint(&ws.path).ok())
                .flatten();
            let text = serde_json::to_string_pretty(&ws.to_json(initial.as_deref()))
                .map_err(|e| format!("cannot serialize the workspace manifest: {e}"))?;
            contain::write_private_atomic(&run_dir.join(workspace::MANIFEST), text)
                .map_err(|e| format!("cannot persist the workspace manifest: {e}"))?;
            if let Some(fp) = &initial {
                log_event(
                    &mut log,
                    json!({"ts": utc_stamp(), "event": "workspace_checkpoint",
                           "workspace_id": ws.id, "at": "run_start", "fingerprint": fp}),
                )?;
            }
        }
    }

    // ---- context snapshot ----
    // P0-04: build_closure, above, pins every `kind: file` context by the
    // bytes it read at that moment - but until now, a step's own context
    // assembly (leaf::prepare_leaf -> context::build) opened the SAME
    // declared path again, live, whenever that step happened to run. See
    // snapshot_file_contexts's doc comment for the bug this closes; this is
    // the run-start call that closes it, placed after the workspace is
    // resolved (immediately above) so a context source that is only
    // contained within the workspace - not the flow directory - validates
    // exactly the way it will for every step that reads it.
    //
    // A resumed run keeps the ORIGINAL snapshot rather than capturing a new
    // one, even under --force-resume. execution-closure.json is itself
    // write-once - resuming never rewrites the copy on disk, force or not
    // (see the closure check above, which reads it back but only ever WRITES
    // it in the `!is_resume` branch) - so re-snapshotting on resume would
    // freeze NEW bytes under a closure that still records the OLD hash: the
    // exact closure/snapshot disagreement this feature must never produce.
    // --force-resume's documented job is "let this run continue despite what
    // moved outside it", not "rebase this run onto the new inputs" - that is
    // what re-running from scratch is for, and a fresh run gets a fresh
    // snapshot the ordinary way, from a fresh closure. See
    // `snapshot_and_persist_context` for why a flow with no `kind: file`
    // context at all leaves no manifest behind on that fresh capture.
    let containment_now = context::Containment {
        flow_dir: &flow_dir,
        workspace: workspace_path.as_deref(),
    };
    let context_snapshot: Option<HashMap<String, Option<PathBuf>>> = if is_resume {
        match load_context_snapshot(&run_dir)? {
            ResumedSnapshot::Loaded(map) => Some(map),
            ResumedSnapshot::NotPresent => None,
            ResumedSnapshot::Corrupt(reason) => {
                if opts.force_resume {
                    log_event(
                        &mut log,
                        json!({"ts": utc_stamp(), "event": "force_resume",
                               "reason": "context_snapshot_unreadable", "error": reason}),
                    )?;
                    if !opts.quiet && !opts.as_json {
                        eprintln!(
                            "sfh: warning: --force-resume continued past an unreadable context snapshot: {reason}"
                        );
                    }
                    None
                } else {
                    return Err(format!(
                        "{}: this run's context snapshot ({}) cannot be read ({reason}), so sfh cannot confirm every step would see the bytes execution-closure.json pinned.\nRe-run from scratch, or pass --force-resume to continue without that guarantee.",
                        machine::ErrorCode::ExecutionClosureChanged.as_str(),
                        run_dir.join(CONTEXT_SNAPSHOT_MANIFEST).display()
                    ));
                }
            }
        }
    } else {
        Some(snapshot_and_persist_context(
            &flow,
            &containment_now,
            &run_dir,
            &mut log,
        )?)
    };
    // Held for the rest of this run: every prepare_leaf call from here on
    // resolves its `kind: file` contexts through this pin instead of the
    // live path. Dropping it (at function exit, or between tests that share
    // a thread) hands the thread back with no pin active.
    let _context_snapshot_guard = context_snapshot.map(context::activate_snapshot);

    let started = utc_stamp();
    let meta = json!({
        "schema_version": 1,
        "sfh_version": VERSION,
        "flow": abs(&opts.flow_path).display().to_string(),
        "flow_fingerprint": flow_fp,
        "flow_fingerprint_algo": FINGERPRINT_ALGO,
        "effective_config_fingerprint": effective_config_fp,
        "effective_config_fingerprint_algo": FINGERPRINT_ALGO,
        "execution_closure_fingerprint": closure_now.fingerprint(),
        "execution_closure_algo": closure::ALGO,
        "name": name,
        "started_utc": started,
        "os": std::env::consts::OS,
        "vars": vars,
        "tools": tool_versions,
        "resumed": is_resume,
        "elapsed_sec": elapsed_before_attempt,
        "workspace": workspace.as_ref().map(|w| json!({
            "mode": w.mode.as_str(),
            "path": w.path.display().to_string(),
            "branch": w.branch,
            "created_by_sfh": w.created_by_sfh,
        })),
        "unsafe_overrides": flow.unsafe_overrides(),
        "profile_overlays": opts.profiles.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        // Absent for a run that started from zero, which is nearly all of them.
        "carried_budget": carried.as_ref().map(CarriedBudget::to_json),
    });
    let meta_text = serde_json::to_string_pretty(&meta)
        .map_err(|e| format!("cannot serialize run metadata: {e}"))?;
    contain::write_private_atomic(&run_dir.join("meta.json"), meta_text)
        .map_err(|e| format!("cannot persist run metadata: {e}"))?;
    log_event(
        &mut log,
        json!({"ts": utc_stamp(), "event": "run_start", "sfh_version": VERSION, "resumed": is_resume, "flow_fingerprint": flow_fp, "effective_config_fingerprint": effective_config_fp}),
    )?;
    // Written as its own durable event, immediately after run_start and before
    // anything is launched. A run that began with someone else's spend on the
    // clock must say so in its own log: otherwise the numbers in status.json
    // are unexplainable, and there is nothing linking the second attempt at a
    // piece of work to the first.
    if let Some(c) = &carried {
        let mut e = c.to_json();
        e["ts"] = json!(utc_stamp());
        e["event"] = json!("budget_carried");
        log_event(&mut log, e)?;
    }

    // ---- live status file + heartbeat so a parent agent can poll liveness ----
    // The run-level activity clock: every child's reader thread stores the
    // moment it read anything here, so the heartbeat can publish "nothing has
    // been said for N minutes" without asking any single step.
    let flow_start = Instant::now();
    let run_clock = Arc::new(AtomicU64::new(0));
    let status_path = run_dir.join("status.json");
    let status = Arc::new(Mutex::new(Status {
        state: "running",
        step: resumed
            .pending_route
            .as_ref()
            .map(|p| p.step.clone())
            .or_else(|| resumed.start.clone())
            .unwrap_or_default(),
        started: started.clone(),
        step_started: None,
        visit: 0,
        last_output: Arc::clone(&run_clock),
        steps_done: resumed.total,
        cost_usd: resumed.cost_usd,
        elapsed_before_attempt,
        attempt_started: flow_start,
        active_members: BTreeMap::new(),
        fanout_total: 0,
        fanout_completed: 0,
        run_dir: run_dir.display().to_string(),
        flow: abs(&opts.flow_path).display().to_string(),
        pid: std::process::id(),
        exit_code: None,
        emit_step: None,
        emit_file: None,
        error: None,
        error_code: None,
        unfinished_step: resumed.unfinished_step.clone(),
        nonce: nonce.clone(),
        pid_start,
    }));
    {
        let g = status.lock().unwrap();
        write_status(&status_path, &g)?;
    }
    let persistence_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    {
        let s = Arc::clone(&status);
        let p = status_path.clone();
        let failed = Arc::clone(&persistence_error);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(3));
            {
                let g = s.lock().unwrap();
                if g.state != "running" {
                    break;
                }
                if let Err(e) = write_status(&p, &g) {
                    if let Ok(mut slot) = failed.lock() {
                        *slot = Some(e);
                    }
                    execute::request_interrupt();
                    break;
                }
            }
        });
    }

    let index_of: HashMap<String, usize> = flow
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone(), i))
        .collect();
    let mut resume_fallback = resumed
        .unfinished_step
        .clone()
        .filter(|unfinished| unfinished.profile.is_some());
    let mut outputs = resumed.outputs;
    let mut visits = resumed.visits;
    let mut sessions = resumed.sessions;
    let mut total: u32 = resumed.total;
    let mut cost_usd: f64 = resumed.cost_usd;
    let mut last_executed = resumed.last_executed;
    let mut last_success = resumed.last_success;
    let mut forced_budget_trigger = resumed.retry_budget_trigger.clone();
    let mut resume_postprocess = resumed
        .pending_route
        .clone()
        .filter(|pending| pending.postprocess);
    let pending_route = if forced_budget_trigger.is_some() {
        None
    } else {
        resumed.pending_route.filter(|pending| !pending.postprocess)
    };
    let completed_members = resumed.completed_members;
    let member_fallbacks = resumed.member_fallbacks;
    let foreach_inputs = resumed.foreach_inputs;
    // step id -> the chain file its LAST visit wrote. A re-visited step writes
    // <id>.v2.chain.txt, so nothing may assume <id>.chain.txt.
    let mut chain_files = resumed.chain_files;
    let resume_id = resumed.start.clone().or_else(|| {
        resume_postprocess
            .as_ref()
            .map(|pending| pending.step.clone())
    });
    let mut cur = match &resume_id {
        Some(id) => *index_of
            .get(id)
            .ok_or_else(|| format!("resume: step '{id}' no longer exists in the flow"))?,
        None => 0,
    };
    if is_resume && !opts.quiet {
        if let Some(p) = &pending_route {
            eprintln!(
                "sfh: resuming {} by replaying pending control flow after step '{}' ({} steps already done, ${cost_usd:.4} spent)",
                run_dir.display(),
                p.step,
                total
            );
        } else if let Some(p) = &resume_postprocess {
            eprintln!(
                "sfh: resuming {} at post-processing for step '{}' visit {} ({} steps already done, ${cost_usd:.4} spent)",
                run_dir.display(),
                p.step,
                p.visit,
                total
            );
        } else {
            eprintln!(
                "sfh: resuming {} at step '{}' ({} steps already done, ${cost_usd:.4} spent)",
                run_dir.display(),
                resumed.start.clone().unwrap_or_default(),
                total
            );
        }
    }
    let max_total = flow.defaults.max_total_steps.unwrap_or(100);
    let n_steps = flow.steps.len();
    let gate = leaf::ToolGate::new(
        flow.defaults
            .tool_max_parallel
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect(),
    );
    // One clock for both the ceiling and the landing threshold, carrying the
    // prior attempt's active time across resume.
    let wall_deadline = flow
        .defaults
        .wall_clock_sec
        .map(|s| flow_start + Duration::from_secs(s.saturating_sub(elapsed_before_attempt)));
    let budget_plan = BudgetPlan::of(&flow.defaults, flow_start, elapsed_before_attempt);
    let mut budget_landed = resumed.budget_landed;

    let result: Result<FlowEnd, String> = (|| {
        if let Some(pending) = pending_route {
            let completed_idx = *index_of.get(&pending.step).ok_or_else(|| {
                format!(
                    "resume: completed step '{}' no longer exists in the flow",
                    pending.step
                )
            })?;
            let step = &flow.steps[completed_idx];
            let replay_route = if pending.errored {
                match apply_on_error(
                    &mut log,
                    step,
                    &index_of,
                    &run_dir,
                    protocol_failure_code(pending.protocol),
                )? {
                    ErrorDisposition::Continue => true,
                    ErrorDisposition::Goto(next) => {
                        cur = next;
                        false
                    }
                    ErrorDisposition::Completed => return Ok(FlowEnd::Completed),
                    ErrorDisposition::Stuck => {
                        return Ok(FlowEnd::Stuck {
                            after: step.id.clone(),
                        })
                    }
                }
            } else {
                true
            };
            if replay_route {
                let gtag = if pending.visit == 1 {
                    step.id.clone()
                } else {
                    format!("{}.v{}", step.id, pending.visit)
                };
                let pf = run_dir.join(format!("{gtag}.prompt.txt"));
                let prep_ctx = leaf::PrepCtx {
                    flow: &flow,
                    vars: &vars,
                    outputs: &outputs,
                    step_ids: &step_ids,
                    run_dir: &run_dir,
                    flow_dir: &flow_dir,
                    notes_file: &notes_file,
                    sessions: &sessions,
                    needed_sessions: &needed_sessions,
                    tainted_vars: &tainted_vars,
                    // Nothing is executed on this path - it only re-evaluates a
                    // recorded route - so there is no child to time.
                    run_clock: None,
                    wall_deadline: None,
                    retry_landing_deadline: None,
                    // The restored spend is real; the clock is this attempt's, the
                    // same one wall_clock_sec is judged on.
                    budget: leaf::BudgetVars::new(
                        &flow.defaults,
                        cost_usd,
                        elapsed_before_attempt.saturating_add(flow_start.elapsed().as_secs()),
                    ),
                    workspace: workspace_path.as_deref(),
                    quiet: opts.quiet,
                    verbose: opts.verbose,
                };
                let builtins = leaf::make_builtins(&prep_ctx, &step.id, pending.visit, &pf, &[]);
                let ctx = template::Ctx {
                    vars: &vars,
                    outputs: &outputs,
                    step_ids: &step_ids,
                    builtins,
                };
                // Which of the "no members" cases this is comes from the FLOW, not
                // from the record: if the flow says this step is a fan-out, its
                // members were countable and a missing snapshot is a gap to refuse,
                // not an empty set to route on.
                //
                // The size has to come from the flow too. A snapshot of N members
                // satisfies `all: true` on its own terms, so a flow edited to
                // declare N+1 (which requires --force-resume, which is exactly when
                // this happens) would report unanimity on a group the new member was
                // never asked about - a live run of the same flow takes the other
                // branch. validate already compares `at_least` against the declared
                // size statically; this is the same comparison at the one moment the
                // two can disagree.
                let members = if let Some(children) = &step.parallel {
                    match &pending.members {
                        Some(m) if m.len() != children.len() => Members::Mismatch {
                            recorded: m.len(),
                            declared: children.len(),
                        },
                        Some(m) => Members::Known(m),
                        None => Members::Unrecorded,
                    }
                } else if step.foreach.is_some() {
                    // A foreach's size is whatever the upstream step produced, so
                    // there is no declared count to check it against.
                    match &pending.members {
                        Some(m) => Members::Known(m),
                        None => Members::Unrecorded,
                    }
                } else {
                    Members::NotAGroup
                };
                let target = evaluate_route(
                    step,
                    &pending.route_text,
                    &ctx,
                    &run_dir,
                    members,
                    pending.protocol,
                )?;
                match target.as_ref().map(|h| (h.goto.as_str(), h)) {
                    None => {
                        log_position(
                            &mut log,
                            &step.id,
                            next_label(completed_idx + 1, &flow),
                            PositionVia::Fallthrough,
                            None,
                        )?;
                        cur = completed_idx + 1;
                        if cur >= n_steps {
                            return Ok(FlowEnd::Completed);
                        }
                    }
                    Some(("end", hit)) => {
                        log_position(&mut log, &step.id, "end".into(), hit.via, Some(hit))?;
                        return Ok(FlowEnd::Completed);
                    }
                    Some(("fail", hit)) => {
                        log_position(&mut log, &step.id, "fail".into(), hit.via, Some(hit))?;
                        return Err(format!("step '{}' routed to fail", step.id));
                    }
                    Some(("stuck", hit)) => {
                        log_position(&mut log, &step.id, "stuck".into(), hit.via, Some(hit))?;
                        return Ok(FlowEnd::Stuck {
                            after: step.id.clone(),
                        });
                    }
                    Some((id, hit)) => {
                        if !opts.quiet {
                            eprintln!("sfh: [{}] -> goto {id}", step.id);
                        }
                        log_position(&mut log, &step.id, id.to_string(), hit.via, Some(hit))?;
                        cur = index_of[id];
                    }
                }
            }
        }
        loop {
            if let Some(e) = persistence_error.lock().ok().and_then(|mut e| e.take()) {
                return Err(e);
            }
            if execute::interrupted() {
                return Err("interrupted (Ctrl+C): child processes were terminated".into());
            }
            if forced_budget_trigger.is_some() && (budget_plan.is_none() || budget_landed) {
                return Err(
                    "resume: a retry was durably pre-empted for budget landing, but the current flow cannot perform that landing"
                        .into(),
                );
            }
            // F5: land before the cliff, once per run. Checked BEFORE the two
            // ceiling checks below, but that head start is worth exactly the
            // reserve and no more: the landing jumps and `continue`s, which
            // brings control straight back HERE, where the untouched ceiling
            // checks now run. A landing with a zero reserve therefore fires on
            // the same values the ceiling is about to fail on and the chain
            // never gets a step - which is why validate refuses a zero reserve
            // on any declared ceiling instead of leaving it to fail silently.
            // After the landing this whole block is skipped and the ceiling
            // checks are all that is left - spend the reserve too and the run
            // ends the way it always did.
            if let (Some(plan), false) = (&budget_plan, budget_landed) {
                let elapsed_sec =
                    elapsed_before_attempt.saturating_add(flow_start.elapsed().as_secs());
                let trigger = forced_budget_trigger
                    .take()
                    .or_else(|| plan.trigger(Instant::now(), cost_usd).map(str::to_string));
                if let Some(trigger) = trigger {
                    budget_landed = true;
                    // The step the landing PRE-EMPTED: it has not run and will
                    // not, unless a resume comes back for it. Recorded as the
                    // position's `after` because that is where the run stood
                    // when the decision was made, and because a landing on
                    // `stuck` resumes from exactly this point.
                    let pending_step = flow.steps[cur].id.clone();
                    log_event(
                        &mut log,
                        json!({"ts": utc_stamp(), "event": "budget_landing", "trigger": trigger,
                               "spent_usd": cost_usd, "elapsed_sec": elapsed_sec,
                               "goto": plan.goto}),
                    )?;
                    if !opts.quiet {
                        eprintln!(
                            "sfh: budget landing ({trigger}): ${cost_usd:.4} spent, {}s elapsed -> goto {}",
                            elapsed_sec,
                            plan.goto
                        );
                    }
                    match plan.goto.as_str() {
                        "end" => {
                            log_position(
                                &mut log,
                                &pending_step,
                                "end".into(),
                                PositionVia::Budget,
                                None,
                            )?;
                            return Ok(FlowEnd::Completed);
                        }
                        "fail" => {
                            log_position(
                                &mut log,
                                &pending_step,
                                "fail".into(),
                                PositionVia::Budget,
                                None,
                            )?;
                            return Err(format!(
                                "on_budget ({trigger}) routed to fail: ${cost_usd:.4} spent, {}s elapsed",
                                elapsed_sec
                            ));
                        }
                        "stuck" => {
                            log_position(
                                &mut log,
                                &pending_step,
                                "stuck".into(),
                                PositionVia::Budget,
                                None,
                            )?;
                            return Ok(FlowEnd::Stuck {
                                after: pending_step,
                            });
                        }
                        id => {
                            log_position(
                                &mut log,
                                &pending_step,
                                id.to_string(),
                                PositionVia::Budget,
                                None,
                            )?;
                            cur = index_of[id];
                            continue;
                        }
                    }
                }
            }
            if let Some(d) = wall_deadline {
                if Instant::now() > d {
                    return Err(format!(
                        "exceeded wall_clock_sec ({})",
                        flow.defaults.wall_clock_sec.unwrap_or(0)
                    ));
                }
            }
            if let Some(limit) = flow.defaults.max_cost_usd {
                if cost_usd >= limit {
                    return Err(format!(
                        "spend guard: ${cost_usd:.4} reported so far, over max_cost_usd (${limit:.4})"
                    ));
                }
            }
            let step = &flow.steps[cur];
            let needs_postprocess =
                step.compact.is_some() || step.notes.as_deref() == Some("append");
            {
                let mut g = status.lock().unwrap();
                g.step = step.id.clone();
                g.steps_done = total;
                g.cost_usd = cost_usd;
                g.active_members.clear();
                g.fanout_total = 0;
                g.fanout_completed = 0;
            }

            let resumed_fallback = if resume_fallback
                .as_ref()
                .is_some_and(|unfinished| unfinished.step == step.id)
            {
                resume_fallback.take()
            } else {
                None
            };
            let resumed_postprocess = if resume_postprocess
                .as_ref()
                .is_some_and(|pending| pending.step == step.id)
            {
                resume_postprocess.take()
            } else {
                None
            };
            let visit = if let Some(visit) = resumed_fallback
                .as_ref()
                .map(|unfinished| unfinished.visit)
                .or_else(|| resumed_postprocess.as_ref().map(|pending| pending.visit))
            {
                visit
            } else {
                visits
                    .get(&step.id)
                    .copied()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or_else(|| {
                        format!("step '{}' exhausted the supported visit counter", step.id)
                    })?
            };
            let max_v = step.max_visits.or(flow.defaults.max_visits).unwrap_or(5);
            // A fallback resumed from its durable checkpoint belongs to the
            // visit that already passed this gate; it neither consumes a new
            // visit nor trips max_visits on re-entry.
            if resumed_fallback.is_none() && resumed_postprocess.is_none() && visit > max_v {
                let action = step.on_max_visits.as_deref().unwrap_or("fail");
                if !opts.quiet {
                    eprintln!(
                        "sfh: [{}] max_visits ({max_v}) exhausted -> {action}",
                        step.id
                    );
                }
                log_event(
                    &mut log,
                    json!({"ts": utc_stamp(), "event": "max_visits", "step": step.id, "action": action}),
                )?;
                match action {
                    "continue" => {
                        log_position(
                            &mut log,
                            &step.id,
                            next_label(cur + 1, &flow),
                            PositionVia::MaxVisits,
                            None,
                        )?;
                        cur += 1;
                        if cur >= n_steps {
                            return Ok(FlowEnd::Completed);
                        }
                        continue;
                    }
                    a if a.starts_with("goto:") => match &a[5..] {
                        "end" => {
                            log_position(
                                &mut log,
                                &step.id,
                                "end".into(),
                                PositionVia::MaxVisits,
                                None,
                            )?;
                            return Ok(FlowEnd::Completed);
                        }
                        "fail" => {
                            log_position(
                                &mut log,
                                &step.id,
                                "fail".into(),
                                PositionVia::MaxVisits,
                                None,
                            )?;
                            return Err(format!("step '{}' exhausted max_visits ({max_v})", step.id));
                        }
                        "stuck" => {
                            log_position(
                                &mut log,
                                &step.id,
                                "stuck".into(),
                                PositionVia::MaxVisits,
                                None,
                            )?;
                            return Ok(FlowEnd::Stuck {
                                after: step.id.clone(),
                            });
                        }
                        id => {
                            log_position(
                                &mut log,
                                &step.id,
                                id.to_string(),
                                PositionVia::MaxVisits,
                                None,
                            )?;
                            cur = index_of[id];
                            continue;
                        }
                    },
                    _ => {
                        return Err(format!(
                            "step '{}' exceeded max_visits ({max_v}) - loop not converging (set on_max_visits: goto:<id> to degrade gracefully). Nodes are checked on entry, so the node entered first in a loop reaches its limit first; put the degradation hook on that node",
                            step.id
                        ))
                    }
                }
            }
            visits.insert(step.id.clone(), visit);
            // Only now is the visit number known, and only a step that actually
            // runs gets a start time: a step turned away by the max_visits gate
            // above never began, and dating it would put a clock on nothing.
            {
                let mut g = status.lock().unwrap();
                g.step_started = Some(utc_stamp());
                g.visit = visit;
            }
            let gtag = if visit == 1 {
                step.id.clone()
            } else {
                format!("{}.v{visit}", step.id)
            };

            // A closure cannot express the borrow lifetimes here; a macro can.
            macro_rules! mk_cx {
                ($outputs:expr, $sessions:expr) => {
                    leaf::PrepCtx {
                        flow: &flow,
                        vars: &vars,
                        outputs: $outputs,
                        step_ids: &step_ids,
                        run_dir: &run_dir,
                        flow_dir: &flow_dir,
                        notes_file: &notes_file,
                        sessions: $sessions,
                        needed_sessions: &needed_sessions,
                        tainted_vars: &tainted_vars,
                        run_clock: Some(&run_clock),
                        wall_deadline,
                        retry_landing_deadline: if budget_landed {
                            None
                        } else {
                            budget_plan.as_ref().and_then(|plan| plan.wall_at)
                        },
                        // Read at expansion time, so every step (and every
                        // retry, fallback and compaction inside it) renders
                        // {{budget.*}} from the totals as they stand now.
                        budget: leaf::BudgetVars::new(
                            &flow.defaults,
                            cost_usd,
                            elapsed_before_attempt.saturating_add(flow_start.elapsed().as_secs()),
                        ),
                        workspace: workspace_path.as_deref(),
                        quiet: opts.quiet,
                        verbose: opts.verbose,
                    }
                };
            }

            // fallback: for a member of a fan-out. Same contract as the plain
            // leaf path - a failed member is re-run under each fallback profile
            // in turn - so a step that fans out does not silently lose the
            // failover it declared. Sequential on purpose: the pool has already
            // finished, and a fallback is the expensive, rare path.
            macro_rules! fan_fallback {
                ($done:expr, $mstep:expr, $label:expr, $mtag:expr, $fbs:expr, $start:expr, $extra:expr) => {{
                    if !$done.ok()
                        && !$done.interrupted
                        && !$done.retry_budget_exhausted
                        && !$fbs.is_empty()
                    {
                        for (fallback_index, fb) in
                            $fbs.iter().enumerate().skip($start)
                        {
                            if execute::interrupted() {
                                return Err(format!(
                                    "interrupted after fan-out member '{}' completed an attempt; its selected fallback is checkpointed for resume",
                                    $label
                                ));
                            }
                            claim_leaf_runs(&mut total, 1, max_total, &step.id)?;
                            if !opts.quiet {
                                eprintln!("sfh: [{}] falling back to profile '{fb}'", $label);
                            }
                            let ftag = format!("{}.fb-{fb}", $mtag);
                            {
                                let mut g = status.lock().unwrap();
                                g.active_members
                                    .insert($label.to_string(), format!("fallback:{fb}"));
                            }
                            let prep = {
                                let cx = mk_cx!(&outputs, &sessions);
                                leaf::prepare_leaf(&cx, $mstep, visit, &ftag, $extra, Some(fb))?
                            };
                            log_event(
                                &mut log,
                                json!({"ts": utc_stamp(), "event": "fallback", "step": $label,
                                       "parent": step.id, "visit": visit, "profile": fb,
                                       "resumed": false, "cmd": prep.inv.describe()}),
                            )?;
                            let alt = leaf::exec_leaf(prep);
                            accumulate_cost(&mut cost_usd, alt.usage.reported_cost());
                            {
                                let mut g = status.lock().unwrap();
                                g.active_members.remove(&$label.to_string());
                                g.cost_usd = cost_usd;
                            }
                            if let Some(e) = &alt.persistence_error {
                                log_persistence_failure(
                                    &mut log,
                                    &$label,
                                    Some(&step.id),
                                    visit,
                                    &alt,
                                    e,
                                )?;
                                return Err(format!("step '{}': {e}", $label));
                            }
                            let ok = alt.ok();
                            let next = if !ok && !alt.interrupted {
                                $fbs.get(fallback_index + 1).map(String::as_str)
                            } else {
                                None
                            };
                            log_step_end_with_next(
                                &mut log,
                                &$label,
                                Some(&step.id),
                                visit,
                                &alt,
                                next,
                                false,
                            )?;
                            if next.is_none() && !alt.interrupted {
                                let mut g = status.lock().unwrap();
                                g.fanout_completed = g.fanout_completed.saturating_add(1);
                            }
                            $done = alt;
                            if ok {
                                break;
                            }
                        }
                    }
                }};
            }

            // ---- execute the step (leaf / parallel / foreach) ----
            // route_text: what route conditions match against - always the
            // pre-compact text, without sfh's "--- id ---" aggregate headers.
            let (
                mut chain_output,
                route_text,
                errored,
                members,
                protocol_state,
                retry_budget_exhausted,
            ): (
                String,
                String,
                bool,
                Option<Vec<MemberVerdict>>,
                Option<protocol::ProtocolState>,
                bool,
            ) = if let Some(pending) = &resumed_postprocess {
                let restored = outputs.get(&step.id).ok_or_else(|| {
                    format!(
                        "resume: step '{}' has pending post-processing but no durable output",
                        step.id
                    )
                })?;
                (
                    restored.output.clone(),
                    pending.route_text.clone(),
                    pending.errored,
                    pending.members.clone(),
                    pending.protocol,
                    false,
                )
            } else if let Some(children) = &step.parallel {
                // Members the crashed attempt already finished: their step_end
                // events restored their output, session and cost, so a resume
                // must NOT prepare or execute them again (rev_regression: the
                // old code rebuilt the whole batch, double-spending, opening
                // duplicate sessions, and counting restored + fresh members
                // against max_total_steps until the resume wedged). A member
                // whose restored output is missing (torn log line) falls back
                // to running.
                let restored: HashSet<String> = completed_members
                    .get(&(step.id.clone(), visit))
                    .map(|set| {
                        set.iter()
                            .filter(|k| outputs.contains_key(*k))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                let cx = mk_cx!(&outputs, &sessions);
                let mut preps = Vec::new();
                let mut fresh_idx: Vec<usize> = Vec::new();
                let mut fallback_starts: Vec<usize> = Vec::new();
                let mut resumed_profiles: Vec<Option<String>> = Vec::new();
                for (ci, c) in children.iter().enumerate() {
                    if restored.contains(&c.id) {
                        continue;
                    }
                    let ctag = if visit == 1 {
                        c.id.clone()
                    } else {
                        format!("{}.v{visit}", c.id)
                    };
                    let resumed_profile = member_fallbacks
                        .get(&(step.id.clone(), visit, c.id.clone()))
                        .cloned();
                    let fallback_start = match &resumed_profile {
                        Some(profile) => c
                            .fallback
                            .iter()
                            .position(|candidate| candidate == profile)
                            .map(|index| index + 1)
                            .ok_or_else(|| {
                                format!(
                                    "resume: fan-out member '{}' checkpointed fallback profile '{}', but the current flow does not list it",
                                    c.id, profile
                                )
                            })?,
                        None => 0,
                    };
                    let prep_tag = resumed_profile
                        .as_ref()
                        .map(|profile| format!("{ctag}.fb-{profile}"))
                        .unwrap_or(ctag);
                    preps.push(leaf::prepare_leaf(
                        &cx,
                        c,
                        visit,
                        &prep_tag,
                        &[],
                        resumed_profile.as_deref(),
                    )?);
                    fresh_idx.push(ci);
                    fallback_starts.push(fallback_start);
                    resumed_profiles.push(resumed_profile);
                }
                claim_leaf_runs(&mut total, preps.len() as u32, max_total, &step.id)?;
                let mp = step
                    .max_parallel
                    .or(flow.defaults.max_parallel)
                    .unwrap_or(4) as usize;
                if !opts.quiet {
                    eprintln!(
                        "sfh: [{}] parallel: {} children ({} restored), max_parallel={mp}",
                        step.id,
                        preps.len(),
                        restored.len()
                    );
                }
                // A skipped member writes no step_end of its own, so THIS log
                // would not remember it and a second crash in the same fan-out
                // would run and re-bill it on the third attempt. Record the
                // carry-over explicitly. It is not a step_end: nothing ran, so
                // it must not add to the leaf count or the cost.
                //
                // BEFORE group_start, and that order is load-bearing. Reading
                // group_start is what cancels the previous lap's carry, so a
                // kill landing between the two lines would drop the members
                // that had not been written yet and run them again. Written
                // first, a kill anywhere in here leaves no group_start at all,
                // the old carry still stands, and nothing is lost.
                log_restored_members(&mut log, &step.id, visit, &restored)?;
                if !preps.is_empty() {
                    log_event(
                        &mut log,
                        json!({"ts": utc_stamp(), "event": "group_start", "step": step.id, "visit": visit, "children": preps.len()}),
                    )?;
                }
                log_member_starts(
                    &mut log,
                    &step.id,
                    visit,
                    preps
                        .iter()
                        .zip(fresh_idx.iter())
                        .map(|(p, &ci)| (children[ci].id.clone(), p)),
                )?;
                for ((prep, &ci), profile) in preps
                    .iter()
                    .zip(fresh_idx.iter())
                    .zip(resumed_profiles.iter())
                {
                    if let Some(profile) = profile {
                        log_event(
                            &mut log,
                            json!({"ts": utc_stamp(), "event": "fallback",
                                   "step": children[ci].id, "parent": step.id,
                                   "visit": visit, "profile": profile,
                                   "resumed": true, "cmd": prep.inv.describe()}),
                        )?;
                    }
                }
                let fresh_labels: Vec<String> = fresh_idx
                    .iter()
                    .map(|&ci| children[ci].id.clone())
                    .collect();
                {
                    let mut g = status.lock().unwrap();
                    g.fanout_total = u32::try_from(children.len()).unwrap_or(u32::MAX);
                    g.fanout_completed = u32::try_from(restored.len()).unwrap_or(u32::MAX);
                    for label in &fresh_labels {
                        g.active_members.insert(label.clone(), "queued".into());
                    }
                }
                let start_labels = fresh_labels.clone();
                let start_status = Arc::clone(&status);
                let mut dones = leaf::run_pool(
                    preps,
                    mp,
                    Arc::clone(&gate),
                    move |pos| {
                        if let Some(label) = start_labels.get(pos) {
                            let mut g = start_status.lock().unwrap();
                            g.active_members.insert(label.clone(), "running".into());
                        }
                    },
                    |pos, done| {
                        let label = fresh_labels.get(pos).ok_or_else(|| {
                            "internal error: fan-out completion index out of range".to_string()
                        })?;
                        let child_index = *fresh_idx.get(pos).ok_or_else(|| {
                            "internal error: fan-out child index out of range".to_string()
                        })?;
                        let next_fallback =
                            if !done.ok() && !done.interrupted && !done.retry_budget_exhausted {
                                children[child_index]
                                    .fallback
                                    .get(fallback_starts[pos])
                                    .map(String::as_str)
                            } else {
                                None
                            };
                        accumulate_cost(&mut cost_usd, done.usage.reported_cost());
                        let durable = match &done.persistence_error {
                            Some(e) => log_persistence_failure(
                                &mut log,
                                label,
                                Some(&step.id),
                                visit,
                                done,
                                e,
                            )
                            .and_then(|_| Err(format!("step '{label}': {e}"))),
                            None => log_step_end_with_next(
                                &mut log,
                                label,
                                Some(&step.id),
                                visit,
                                done,
                                next_fallback,
                                false,
                            ),
                        };
                        {
                            let mut g = status.lock().unwrap();
                            g.active_members.remove(label);
                            g.cost_usd = cost_usd;
                            if durable.is_ok() && next_fallback.is_none() && !done.interrupted {
                                g.fanout_completed = g.fanout_completed.saturating_add(1);
                            }
                        }
                        durable
                    },
                )?;
                for (pos, &ci) in fresh_idx.iter().enumerate() {
                    let c = &children[ci];
                    let ctag = if visit == 1 {
                        c.id.clone()
                    } else {
                        format!("{}.v{visit}", c.id)
                    };
                    fan_fallback!(
                        dones[pos],
                        c,
                        c.id,
                        ctag,
                        &c.fallback,
                        fallback_starts[pos],
                        &[]
                    );
                }
                let mut agg = String::new();
                let mut plain = String::new();
                let mut verdicts: Vec<MemberVerdict> = Vec::with_capacity(children.len());
                let mut hard_fail = false;
                let mut di = 0usize;
                for c in children.iter() {
                    if restored.contains(&c.id) {
                        // Restored member: its cost was already counted and its
                        // step_end already logged by the crashed attempt; only
                        // the aggregate text is rebuilt. The stored output is
                        // the exposed (failure-wrapped) form, so it goes into
                        // both agg and plain as-is; for a failed member plain
                        // therefore differs slightly from what the live run
                        // matched against (raw text), which only matters when
                        // the group routes on despite a failed member.
                        let so = outputs.get(&c.id).expect("filtered above");
                        if so.exit != 0 && c.on_error.as_deref() != Some("continue") {
                            hard_fail = true;
                        }
                        // Only members the log recorded as ok are ever carried
                        // over, so this text is the member's own output with no
                        // failure wrapper around it - the same bytes the live
                        // path votes on. The exit test is belt and braces.
                        verdicts.push(MemberVerdict::new(&c.id, so.exit == 0, so.exit, &so.output));
                        agg.push_str(&format!("--- {} ---\n{}\n\n", c.id, so.output.trim_end()));
                        plain.push_str(&format!("{}\n\n", so.output.trim_end()));
                        continue;
                    }
                    let d = &dones[di];
                    di += 1;
                    let exposed = if d.ok() {
                        d.chain_output.clone()
                    } else {
                        failed_output(&c.id, &d.chain_output, d.exit_code, d.timed_out)
                    };
                    if !d.ok() {
                        eprintln!(
                            "sfh: [{}] failed (exit={}, timed_out={})",
                            d.tag, d.exit_code, d.timed_out
                        );
                        for line in leaf::tail_lines(&d.stderr_clean, 10) {
                            eprintln!("sfh: [{}] stderr| {line}", d.tag);
                        }
                        if c.on_error.as_deref() != Some("continue") {
                            hard_fail = true;
                        }
                    }
                    outputs.insert(
                        c.id.clone(),
                        template::StepOutput {
                            output: exposed.clone(),
                            outputs: exposed.clone(),
                            output_file: d.out_file.display().to_string(),
                            exit: d.exit_code,
                            stderr_file: stderr_file_for(&d.out_file).display().to_string(),
                            outcome: outcome_name(d),
                            label: outcome_label(d),
                        },
                    );
                    if let (Some(tool), Some(sid)) = (&d.tool, &d.session_id) {
                        sessions.insert(
                            c.id.clone(),
                            leaf::SessionInfo {
                                tool: tool.clone(),
                                id: sid.clone(),
                                cwd: d.cwd.clone(),
                                marker: d.session_marker.clone(),
                                access: d.access,
                                // Opened by this process, so the level is
                                // first-hand rather than read back from a log.
                                access_recorded: true,
                            },
                        );
                    }
                    let failed_header = if d.ok() {
                        String::new()
                    } else {
                        format!(
                            " [sfh: FAILED exit={}, timed_out={}]",
                            d.exit_code, d.timed_out
                        )
                    };
                    // From the child's OWN output and the engine's own verdict
                    // on it, not from `exposed` and not from the aggregate: the
                    // aggregate is where the two become indistinguishable.
                    verdicts.push(MemberVerdict::new(
                        &c.id,
                        d.ok(),
                        d.exit_code,
                        &d.chain_output,
                    ));
                    agg.push_str(&format!(
                        "--- {}{} ---\n{}\n\n",
                        c.id,
                        failed_header,
                        exposed.trim_end()
                    ));
                    plain.push_str(&format!("{}\n\n", d.chain_output.trim_end()));
                }
                let agg = agg.trim_end().to_string();
                let plain = plain.trim_end().to_string();
                // The headerless routing text, kept separately: the chain file
                // holds the labeled aggregate, and a resume must route against
                // exactly what this live run routed against (see load_resume).
                let plain_name = format!("{gtag}.plain.txt");
                let plain_path = run_dir.join(&plain_name);
                contain::write_private_atomic(&plain_path, &plain).map_err(|e| {
                    format!(
                        "cannot persist aggregate routing text {}: {e}",
                        plain_path.display()
                    )
                })?;
                write_aggregate(&run_dir, &gtag, &agg, &mut outputs, &step.id, hard_fail)?;
                log_aggregate_end(
                    &mut log,
                    AggregateEnd {
                        step: &step.id,
                        visit,
                        gtag: &gtag,
                        failed: hard_fail,
                        plain: &plain,
                        plain_file: &plain_name,
                        members: &verdicts,
                        postprocess_pending: !hard_fail && needs_postprocess,
                    },
                )?;
                (
                    agg,
                    plain,
                    hard_fail,
                    Some(verdicts),
                    None,
                    dones.iter().any(|done| done.retry_budget_exhausted),
                )
            } else if let Some(fe) = &step.foreach {
                let cx = mk_cx!(&outputs, &sessions);
                let pf = run_dir.join(format!("{gtag}.from.txt"));
                let builtins = leaf::make_builtins(&cx, &step.id, visit, &pf, &[]);
                let tctx = template::Ctx {
                    vars: &vars,
                    outputs: &outputs,
                    step_ids: &step_ids,
                    builtins,
                };
                let from = template::render(&fe.from, &tctx)
                    .map_err(|e| format!("step '{}' foreach.from: {e}", step.id))?;
                let items = split_items(&from, fe.split.as_deref())
                    .map_err(|e| format!("step '{}': {e}", step.id))?;
                if items.len() > 100 {
                    return Err(format!(
                        "step '{}': foreach produced {} items (max 100) - check the split",
                        step.id,
                        items.len()
                    ));
                }
                if items.is_empty() {
                    eprintln!("sfh: warning: step '{}': foreach produced 0 items", step.id);
                }
                let items_hash = foreach_items_hash(&items);
                if let Some(expected) = foreach_inputs.get(&(step.id.clone(), visit)) {
                    if expected != &items_hash {
                        return Err(format!(
                            "resume: foreach inputs for step '{}' visit {} changed; refusing to apply completed item indexes to a different ordered list (recorded {}, current {}). Restore the original vars/inputs, or start a new run",
                            step.id, visit, expected, items_hash
                        ));
                    }
                }
                // Items the crashed attempt already finished, keyed by the
                // "id[i]" label they logged under; skipped, not re-run
                // (rev_regression - see the parallel branch above). Item order
                // is re-derived from the same template and restored inputs, so
                // the indices line up; an item whose restored output is missing
                // falls back to running.
                let restored: HashSet<String> = completed_members
                    .get(&(step.id.clone(), visit))
                    .map(|set| {
                        set.iter()
                            .filter(|k| outputs.contains_key(*k))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                let mut preps = Vec::new();
                let mut fresh_idx: Vec<usize> = Vec::new();
                let mut fallback_starts: Vec<usize> = Vec::new();
                let mut resumed_profiles: Vec<Option<String>> = Vec::new();
                for (i, it) in items.iter().enumerate() {
                    let label = format!("{}[{i}]", step.id);
                    if restored.contains(&label) {
                        continue;
                    }
                    let tag = if visit == 1 {
                        format!("{}.i{i}", step.id)
                    } else {
                        format!("{}.v{visit}.i{i}", step.id)
                    };
                    let resumed_profile = member_fallbacks
                        .get(&(step.id.clone(), visit, label.clone()))
                        .cloned();
                    let fallback_start = match &resumed_profile {
                        Some(profile) => step
                            .fallback
                            .iter()
                            .position(|candidate| candidate == profile)
                            .map(|index| index + 1)
                            .ok_or_else(|| {
                                format!(
                                    "resume: foreach member '{label}' checkpointed fallback profile '{profile}', but the current flow does not list it"
                                )
                            })?,
                        None => 0,
                    };
                    let prep_tag = resumed_profile
                        .as_ref()
                        .map(|profile| format!("{tag}.fb-{profile}"))
                        .unwrap_or(tag);
                    preps.push(leaf::prepare_leaf(
                        &cx,
                        step,
                        visit,
                        &prep_tag,
                        &[("item", it.clone()), ("item_index", i.to_string())],
                        resumed_profile.as_deref(),
                    )?);
                    fresh_idx.push(i);
                    fallback_starts.push(fallback_start);
                    resumed_profiles.push(resumed_profile);
                }
                claim_leaf_runs(&mut total, preps.len() as u32, max_total, &step.id)?;
                let mp = step
                    .max_parallel
                    .or(flow.defaults.max_parallel)
                    .unwrap_or(4) as usize;
                if !opts.quiet {
                    eprintln!(
                        "sfh: [{}] foreach: {} items ({} restored), max_parallel={mp}",
                        step.id,
                        preps.len(),
                        restored.len()
                    );
                }
                // See the parallel branch, including why this goes BEFORE the
                // foreach_start line: reading that line is what cancels the
                // previous lap's carry.
                log_restored_members(&mut log, &step.id, visit, &restored)?;
                if !preps.is_empty() {
                    log_event(
                        &mut log,
                        json!({"ts": utc_stamp(), "event": "foreach_start", "step": step.id,
                               "visit": visit, "items": preps.len(), "items_total": items.len(),
                               "items_hash": items_hash}),
                    )?;
                }
                log_member_starts(
                    &mut log,
                    &step.id,
                    visit,
                    preps
                        .iter()
                        .zip(fresh_idx.iter())
                        .map(|(p, &i)| (format!("{}[{i}]", step.id), p)),
                )?;
                for ((prep, &i), profile) in preps
                    .iter()
                    .zip(fresh_idx.iter())
                    .zip(resumed_profiles.iter())
                {
                    if let Some(profile) = profile {
                        log_event(
                            &mut log,
                            json!({"ts": utc_stamp(), "event": "fallback",
                                   "step": format!("{}[{i}]", step.id), "parent": step.id,
                                   "visit": visit, "profile": profile,
                                   "resumed": true, "cmd": prep.inv.describe()}),
                        )?;
                    }
                }
                let fresh_labels: Vec<String> = fresh_idx
                    .iter()
                    .map(|&i| format!("{}[{i}]", step.id))
                    .collect();
                {
                    let mut g = status.lock().unwrap();
                    g.fanout_total = u32::try_from(items.len()).unwrap_or(u32::MAX);
                    g.fanout_completed = u32::try_from(restored.len()).unwrap_or(u32::MAX);
                    for label in &fresh_labels {
                        g.active_members.insert(label.clone(), "queued".into());
                    }
                }
                let start_labels = fresh_labels.clone();
                let start_status = Arc::clone(&status);
                let mut dones = leaf::run_pool(
                    preps,
                    mp,
                    Arc::clone(&gate),
                    move |pos| {
                        if let Some(label) = start_labels.get(pos) {
                            let mut g = start_status.lock().unwrap();
                            g.active_members.insert(label.clone(), "running".into());
                        }
                    },
                    |pos, done| {
                        let label = fresh_labels.get(pos).ok_or_else(|| {
                            "internal error: foreach completion index out of range".to_string()
                        })?;
                        let next_fallback =
                            if !done.ok() && !done.interrupted && !done.retry_budget_exhausted {
                                step.fallback.get(fallback_starts[pos]).map(String::as_str)
                            } else {
                                None
                            };
                        accumulate_cost(&mut cost_usd, done.usage.reported_cost());
                        let durable = match &done.persistence_error {
                            Some(e) => log_persistence_failure(
                                &mut log,
                                label,
                                Some(&step.id),
                                visit,
                                done,
                                e,
                            )
                            .and_then(|_| Err(format!("step '{label}': {e}"))),
                            None => log_step_end_with_next(
                                &mut log,
                                label,
                                Some(&step.id),
                                visit,
                                done,
                                next_fallback,
                                false,
                            ),
                        };
                        {
                            let mut g = status.lock().unwrap();
                            g.active_members.remove(label);
                            g.cost_usd = cost_usd;
                            if durable.is_ok() && next_fallback.is_none() && !done.interrupted {
                                g.fanout_completed = g.fanout_completed.saturating_add(1);
                            }
                        }
                        durable
                    },
                )?;
                for (pos, &i) in fresh_idx.iter().enumerate() {
                    let tag = if visit == 1 {
                        format!("{}.i{i}", step.id)
                    } else {
                        format!("{}.v{visit}.i{i}", step.id)
                    };
                    let extra = [
                        ("item", items.get(i).cloned().unwrap_or_default()),
                        ("item_index", i.to_string()),
                    ];
                    let label = format!("{}[{i}]", step.id);
                    fan_fallback!(
                        dones[pos],
                        step,
                        label,
                        tag,
                        &step.fallback,
                        fallback_starts[pos],
                        &extra
                    );
                }
                let mut agg = String::new();
                let mut plain = String::new();
                let mut verdicts: Vec<MemberVerdict> = Vec::with_capacity(items.len());
                let mut any_fail = false;
                let mut di = 0usize;
                #[allow(clippy::needless_range_loop)]
                for i in 0..items.len() {
                    let label = format!("{}[{i}]", step.id);
                    if restored.contains(&label) {
                        // Restored item: cost counted and step_end logged by the
                        // crashed attempt; rebuild the aggregate text only (the
                        // same plain-text caveat as the parallel branch applies).
                        let so = outputs.get(&label).expect("filtered above");
                        if so.exit != 0 {
                            any_fail = true;
                        }
                        // See the parallel branch: a carried-over item was
                        // recorded ok, so its stored text carries no wrapper.
                        verdicts.push(MemberVerdict::new(
                            &label,
                            so.exit == 0,
                            so.exit,
                            &so.output,
                        ));
                        agg.push_str(&format!(
                            "--- {}[{i}] item: {} ---\n{}\n\n",
                            step.id,
                            one_line(items.get(i).map(String::as_str).unwrap_or(""), 80),
                            so.output.trim_end()
                        ));
                        plain.push_str(&format!("{}\n\n", so.output.trim_end()));
                        continue;
                    }
                    let d = &dones[di];
                    di += 1;
                    let exposed = if d.ok() {
                        d.chain_output.clone()
                    } else {
                        failed_output(&label, &d.chain_output, d.exit_code, d.timed_out)
                    };
                    if !d.ok() {
                        any_fail = true;
                        eprintln!(
                            "sfh: [{}] failed (exit={}, timed_out={})",
                            d.tag, d.exit_code, d.timed_out
                        );
                        for line in leaf::tail_lines(&d.stderr_clean, 10) {
                            eprintln!("sfh: [{}] stderr| {line}", d.tag);
                        }
                    }
                    let failed_header = if d.ok() {
                        String::new()
                    } else {
                        format!(
                            " [sfh: FAILED exit={}, timed_out={}]",
                            d.exit_code, d.timed_out
                        )
                    };
                    verdicts.push(MemberVerdict::new(
                        &label,
                        d.ok(),
                        d.exit_code,
                        &d.chain_output,
                    ));
                    agg.push_str(&format!(
                        "--- {}[{i}]{} item: {} ---\n{}\n\n",
                        step.id,
                        failed_header,
                        one_line(items.get(i).map(String::as_str).unwrap_or(""), 80),
                        exposed.trim_end()
                    ));
                    plain.push_str(&format!("{}\n\n", d.chain_output.trim_end()));
                }
                // A single failed item is fatal unless the step opted out.
                let hard_fail = any_fail && step.on_error.as_deref() != Some("continue");
                let agg = agg.trim_end().to_string();
                let plain = plain.trim_end().to_string();
                let plain_name = format!("{gtag}.plain.txt");
                let plain_path = run_dir.join(&plain_name);
                contain::write_private_atomic(&plain_path, &plain).map_err(|e| {
                    format!(
                        "cannot persist aggregate routing text {}: {e}",
                        plain_path.display()
                    )
                })?;
                write_aggregate(&run_dir, &gtag, &agg, &mut outputs, &step.id, hard_fail)?;
                log_aggregate_end(
                    &mut log,
                    AggregateEnd {
                        step: &step.id,
                        visit,
                        gtag: &gtag,
                        failed: hard_fail,
                        plain: &plain,
                        plain_file: &plain_name,
                        members: &verdicts,
                        postprocess_pending: !hard_fail && needs_postprocess,
                    },
                )?;
                (
                    agg,
                    plain,
                    hard_fail,
                    Some(verdicts),
                    None,
                    dones.iter().any(|done| done.retry_budget_exhausted),
                )
            } else {
                let mut next_fallback_index = 0usize;
                let mut done = if let Some(unfinished) = &resumed_fallback {
                    let profile = unfinished.profile.as_deref().ok_or_else(|| {
                        "internal error: fallback checkpoint has no profile".to_string()
                    })?;
                    let index = step
                        .fallback
                        .iter()
                        .position(|candidate| candidate == profile)
                        .ok_or_else(|| {
                            format!(
                                "resume: step '{}' checkpointed fallback profile '{}', but the current flow does not list it",
                                step.id, profile
                            )
                        })?;
                    next_fallback_index = index + 1;
                    claim_leaf_runs(&mut total, 1, max_total, &step.id)?;
                    if !opts.quiet {
                        eprintln!("sfh: [{}] resuming fallback profile '{profile}'", step.id);
                    }
                    let ftag = format!("{gtag}.fb-{profile}");
                    let cx = mk_cx!(&outputs, &sessions);
                    let prep = leaf::prepare_leaf(&cx, step, visit, &ftag, &[], Some(profile))?;
                    log_event(
                        &mut log,
                        json!({"ts": utc_stamp(), "event": "fallback", "step": step.id,
                               "visit": visit, "profile": profile, "resumed": true,
                               "cmd": prep.inv.describe()}),
                    )?;
                    leaf::exec_leaf(prep)
                } else {
                    claim_leaf_runs(&mut total, 1, max_total, &step.id)?;
                    let cx = mk_cx!(&outputs, &sessions);
                    let prep = leaf::prepare_leaf(&cx, step, visit, &gtag, &[], None)?;
                    log_event(
                        &mut log,
                        json!({"ts": utc_stamp(), "event": "step_start", "step": step.id, "visit": visit, "cmd": prep.inv.describe(), "session_parent": session_parent_json(&prep), "protocol_expected": protocol::expected_kind(&prep.parse),
                               "context_hash": prep.context_hash, "context_file": prep.context_file.as_ref().and_then(|p| file_name(p)),
                               "workspace_id": workspace.as_ref().map(|w| w.id.clone())}),
                    )?;
                    leaf::exec_leaf(prep)
                };
                // Usage belongs to the attempt even when publishing one of its
                // required artifacts failed. Account it before every possible
                // durability error so run_end/meta never erase paid work.
                accumulate_cost(&mut cost_usd, done.usage.reported_cost());
                if let Some(e) = &done.persistence_error {
                    log_persistence_failure(&mut log, &step.id, None, visit, &done, e)?;
                    return Err(format!("step '{}': {e}", step.id));
                }
                // fallback: retry the step with a different profile (tool/model).
                if !done.ok()
                    && !done.interrupted
                    && !done.retry_budget_exhausted
                    && !step.fallback.is_empty()
                {
                    for fb in step.fallback.iter().skip(next_fallback_index) {
                        log_step_end_with_next(
                            &mut log,
                            &step.id,
                            None,
                            visit,
                            &done,
                            Some(fb),
                            false,
                        )?;
                        // The paid attempt and selected next profile are one
                        // durable checkpoint. An interrupt that lands after the
                        // child exits must not turn that completed failure into
                        // an uncheckpointed primary that resume runs again.
                        if execute::interrupted() {
                            return Err(
                                "interrupted after a completed attempt; the selected fallback is checkpointed for resume"
                                    .into(),
                            );
                        }
                        // The completed attempt above is already paid work and
                        // its checkpoint must become durable even when the next
                        // fallback would exceed max_total_steps. Claiming first
                        // used to return early with only step_start recorded,
                        // so resume reran and rebilled the completed primary.
                        claim_leaf_runs(&mut total, 1, max_total, &step.id)?;
                        if !opts.quiet {
                            eprintln!("sfh: [{}] falling back to profile '{fb}'", step.id);
                        }
                        let ftag = format!("{gtag}.fb-{fb}");
                        let cx = mk_cx!(&outputs, &sessions);
                        let prep = leaf::prepare_leaf(&cx, step, visit, &ftag, &[], Some(fb))?;
                        log_event(
                            &mut log,
                            json!({"ts": utc_stamp(), "event": "fallback", "step": step.id,
                                   "visit": visit, "profile": fb, "resumed": false,
                                   "cmd": prep.inv.describe()}),
                        )?;
                        let alt = leaf::exec_leaf(prep);
                        accumulate_cost(&mut cost_usd, alt.usage.reported_cost());
                        if let Some(e) = &alt.persistence_error {
                            log_persistence_failure(&mut log, &step.id, None, visit, &alt, e)?;
                            return Err(format!("step '{}': {e}", step.id));
                        }
                        let ok = alt.ok();
                        done = alt;
                        if ok {
                            break;
                        }
                    }
                }
                let d = &done;
                if !d.ok() {
                    eprintln!(
                        "sfh: [{}] failed (exit={}, timed_out={})",
                        d.tag, d.exit_code, d.timed_out
                    );
                    for line in leaf::tail_lines(&d.stderr_clean, 15) {
                        eprintln!("sfh: [{}] stderr| {line}", d.tag);
                    }
                }
                if let (Some(tool), Some(sid)) = (&d.tool, &d.session_id) {
                    sessions.insert(
                        step.id.clone(),
                        leaf::SessionInfo {
                            tool: tool.clone(),
                            id: sid.clone(),
                            cwd: d.cwd.clone(),
                            marker: d.session_marker.clone(),
                            access: d.access,
                            // Opened by this process: first-hand.
                            access_recorded: true,
                        },
                    );
                }
                log_step_end_with_next(
                    &mut log,
                    &step.id,
                    None,
                    visit,
                    d,
                    None,
                    d.ok() && needs_postprocess,
                )?;
                let exposed = if d.ok() {
                    d.chain_output.clone()
                } else {
                    failed_output(&step.id, &d.chain_output, d.exit_code, d.timed_out)
                };
                outputs.insert(
                    step.id.clone(),
                    template::StepOutput {
                        output: exposed.clone(),
                        outputs: exposed,
                        output_file: d.out_file.display().to_string(),
                        exit: d.exit_code,
                        stderr_file: stderr_file_for(&d.out_file).display().to_string(),
                        outcome: outcome_name(d),
                        label: outcome_label(d),
                    },
                );
                let rt = d.chain_output.clone();
                // A leaf has no members, which is not the same as having none
                // recorded - see `Members`.
                (
                    d.chain_output.clone(),
                    rt,
                    !d.ok(),
                    None,
                    Some(d.protocol.protocol),
                    d.retry_budget_exhausted,
                )
            };
            if let Some(e) = persistence_error.lock().ok().and_then(|mut e| e.take()) {
                return Err(e);
            }
            // Cancellation is stronger than on_error, routing, compact and
            // notes. The interrupted result above is durable diagnostic state,
            // but it is deliberately not a completed step and must never route
            // to `end` (or append/compact partial output) before the loop-top
            // interrupt check gets another chance to run.
            if execute::interrupted() {
                return Err("interrupted (Ctrl+C): child processes were terminated".into());
            }
            // A failed attempt is durable above, but its next retry was
            // pre-empted when backoff entered the wall-clock reserve. Budget
            // landing outranks fallback/on_error and is evaluated at the loop
            // head, using the same one-shot event/position path as any other
            // landing. The step remains current so the position records what
            // work the landing pre-empted.
            if retry_budget_exhausted {
                forced_budget_trigger = Some("wall_clock".into());
                continue;
            }
            // A run-level deadline is stronger than a leaf's on_error policy.
            // Each leaf timeout is capped to this instant, so report the
            // global budget cause instead of misclassifying that kill as an
            // ordinary step failure.
            if wall_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(format!(
                    "exceeded wall_clock_sec ({})",
                    flow.defaults.wall_clock_sec.unwrap_or(0)
                ));
            }
            let notes_output = resumed_postprocess
                .as_ref()
                .and_then(|_| outputs.get(&step.id))
                .map(|output| output.outputs.clone())
                .unwrap_or_else(|| chain_output.clone());
            let compact_already_done = resumed_postprocess
                .as_ref()
                .is_some_and(|pending| pending.compact_done);
            let notes_already_done = resumed_postprocess
                .as_ref()
                .is_some_and(|pending| pending.notes_done);

            // ---- compact ----
            if let Some(comp) = &step.compact {
                if !compact_already_done
                    && !errored
                    && chain_output.chars().count() as u64 > comp.when_over
                {
                    if !opts.quiet {
                        eprintln!(
                            "sfh: [{}] compacting output ({} chars > {})",
                            step.id,
                            chain_output.chars().count(),
                            comp.when_over
                        );
                    }
                    claim_leaf_runs(&mut total, 1, max_total, &step.id)?;
                    // Keep what is about to be summarized: the chain file gets
                    // overwritten with the summary, and {{steps.X.outputs}} is
                    // documented as the pre-compact original - including after
                    // a --resume, which can only read files.
                    let pre_name = format!("{gtag}.precompact.txt");
                    let pre_path = run_dir.join(&pre_name);
                    contain::write_private_sync(&pre_path, &chain_output).map_err(|e| {
                        format!(
                            "cannot persist pre-compact artifact {}: {e}",
                            pre_path.display()
                        )
                    })?;
                    log_event(
                        &mut log,
                        json!({"ts": utc_stamp(), "event": "compact_start", "step": step.id,
                               "visit": visit, "chars": chain_output.chars().count(),
                               "precompact_file": &pre_name}),
                    )?;
                    let cx = mk_cx!(&outputs, &sessions);
                    let compact_prompt_file = run_dir.join(format!("{gtag}.compact.prompt.txt"));
                    let builtins =
                        leaf::make_builtins(&cx, &step.id, visit, &compact_prompt_file, &[]);
                    let compact_ctx = template::Ctx {
                        vars: &vars,
                        outputs: &outputs,
                        step_ids: &step_ids,
                        builtins,
                    };
                    let compact_run = CompactRun {
                        flow: &flow,
                        ctx: &compact_ctx,
                        original: &chain_output,
                        run_dir: &run_dir,
                        tag: &gtag,
                        run_clock: &run_clock,
                        wall_deadline,
                        workspace: workspace_path.as_deref(),
                        quiet: opts.quiet,
                        verbose: opts.verbose,
                    };
                    let compact_outcome =
                        run_compact(comp, compact_run).unwrap_or_else(|error| CompactOutcome {
                            summary: Err(error),
                            usage: preset::Usage::default(),
                        });
                    let compact_cost = compact_outcome.usage.reported_cost();
                    accumulate_cost(&mut cost_usd, compact_cost);
                    let chain_name = format!("{gtag}.chain.txt");
                    let compact_event = match compact_outcome.summary {
                        Ok(sum) => {
                            chain_output = sum;
                            json!({
                                "ts": utc_stamp(),
                                "event": "compact_end",
                                "step": step.id,
                                "visit": visit,
                                "chars": chain_output.chars().count(),
                                "cost_usd": compact_cost,
                                "output_hash": fingerprint(&chain_output),
                                "precompact_file": pre_name,
                                "chain_file": chain_name,
                            })
                        }
                        Err(e) => {
                            eprintln!(
                                "sfh: warning: step '{}' compact failed ({e}); using head+tail of the original",
                                step.id
                            );
                            chain_output = head_tail(&chain_output, comp.when_over as usize);
                            json!({
                                "ts": utc_stamp(),
                                "event": "compact_failed",
                                "step": step.id,
                                "visit": visit,
                                "chars": chain_output.chars().count(),
                                "error": e,
                                "cost_usd": compact_cost,
                                "output_hash": fingerprint(&chain_output),
                                "precompact_file": pre_name,
                                "chain_file": chain_name,
                            })
                        }
                    };
                    // The checkpoint may only become visible after the chain
                    // it names is on stable storage. The old event-first order
                    // let a crash replay a paid summarizer or restore stale
                    // text from the leaf's original chain.
                    let compact_chain = run_dir.join(&chain_name);
                    contain::write_private_atomic(&compact_chain, &chain_output).map_err(|e| {
                        format!(
                            "cannot persist compacted chain artifact {}: {e}",
                            compact_chain.display()
                        )
                    })?;
                    if let Some(entry) = outputs.get_mut(&step.id) {
                        entry.output = chain_output.clone();
                    }
                    log_event(&mut log, compact_event)?;
                }
            }
            if wall_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(format!(
                    "exceeded wall_clock_sec ({})",
                    flow.defaults.wall_clock_sec.unwrap_or(0)
                ));
            }

            // A re-visited step writes <id>.v2.chain.txt, so the emitted file
            // has to follow the visit rather than assume <id>.chain.txt.
            chain_files.insert(step.id.clone(), run_dir.join(format!("{gtag}.chain.txt")));

            // ---- notes ----
            if step.notes.as_deref() == Some("append") && !errored && !notes_already_done {
                let marker = note_marker(&step.id, visit, &notes_output);
                write_note_once(
                    &run_dir,
                    &notes_file,
                    &marker,
                    &step.id,
                    visit,
                    &notes_output,
                )?;
                log_event(
                    &mut log,
                    json!({
                        "ts": utc_stamp(),
                        "event": "notes_end",
                        "step": step.id,
                        "visit": visit,
                        "marker": marker,
                    }),
                )?;
            }
            if !errored && (needs_postprocess || resumed_postprocess.is_some()) {
                log_event(
                    &mut log,
                    json!({"ts": utc_stamp(), "event": "postprocess_end",
                           "step": step.id, "visit": visit}),
                )?;
            }

            last_executed = Some(step.id.clone());
            if !errored {
                last_success = Some(step.id.clone());
            }
            {
                let mut g = status.lock().unwrap();
                g.steps_done = total;
                g.cost_usd = cost_usd;
            }

            // ---- error handling ----
            if errored {
                match apply_on_error(
                    &mut log,
                    step,
                    &index_of,
                    &run_dir,
                    protocol_failure_code(protocol_state),
                )? {
                    ErrorDisposition::Continue => {}
                    ErrorDisposition::Goto(next) => {
                        cur = next;
                        continue;
                    }
                    ErrorDisposition::Completed => return Ok(FlowEnd::Completed),
                    ErrorDisposition::Stuck => {
                        return Ok(FlowEnd::Stuck {
                            after: step.id.clone(),
                        })
                    }
                }
            }

            // ---- routing ----
            let target = {
                let pf = run_dir.join(format!("{gtag}.prompt.txt"));
                let cx = mk_cx!(&outputs, &sessions);
                let builtins = leaf::make_builtins(&cx, &step.id, visit, &pf, &[]);
                let ctx = template::Ctx {
                    vars: &vars,
                    outputs: &outputs,
                    step_ids: &step_ids,
                    builtins,
                };
                evaluate_route(
                    step,
                    &route_text,
                    &ctx,
                    &run_dir,
                    match &members {
                        Some(m) => Members::Known(m),
                        None => Members::NotAGroup,
                    },
                    protocol_state,
                )?
            };
            match target.as_ref().map(|h| (h.goto.as_str(), h)) {
                None => {
                    log_position(
                        &mut log,
                        &step.id,
                        next_label(cur + 1, &flow),
                        PositionVia::Fallthrough,
                        None,
                    )?;
                    cur += 1;
                    if cur >= n_steps {
                        return Ok(FlowEnd::Completed);
                    }
                }
                Some(("end", hit)) => {
                    log_position(&mut log, &step.id, "end".into(), hit.via, Some(hit))?;
                    return Ok(FlowEnd::Completed);
                }
                Some(("fail", hit)) => {
                    log_position(&mut log, &step.id, "fail".into(), hit.via, Some(hit))?;
                    return Err(format!("step '{}' routed to fail", step.id));
                }
                Some(("stuck", hit)) => {
                    log_position(&mut log, &step.id, "stuck".into(), hit.via, Some(hit))?;
                    return Ok(FlowEnd::Stuck {
                        after: step.id.clone(),
                    });
                }
                Some((id, hit)) => {
                    if !opts.quiet {
                        eprintln!("sfh: [{}] -> goto {id}", step.id);
                    }
                    log_position(&mut log, &step.id, id.to_string(), hit.via, Some(hit))?;
                    cur = index_of[id];
                }
            }
        }
    })();

    let max_emit = flow.defaults.max_emit_chars.unwrap_or(200_000) as usize;
    let finish = |state: &'static str,
                  cost: f64,
                  code: i32,
                  emit: Option<&str>,
                  err: Option<&str>|
     -> Result<(), String> {
        let mut g = status.lock().unwrap();
        g.state = state;
        g.steps_done = total;
        g.cost_usd = cost;
        g.exit_code = Some(code);
        g.emit_step = emit.map(String::from);
        g.emit_file = emit
            .and_then(|id| chain_files.get(id))
            .map(|p| p.display().to_string());
        g.error = err.map(String::from);
        g.error_code = match state {
            "stuck" => Some(machine::ErrorCode::Stuck.as_str().to_string()),
            "failed" => err
                .map(run_failure_code)
                .map(|code| code.as_str().to_string()),
            "stopped" | "dead" => Some(machine::ErrorCode::Interrupted.as_str().to_string()),
            _ => None,
        };
        write_status(&status_path, &g)
    };
    // The output a caller gets when the run did NOT succeed. Computed once,
    // before the match, because `stuck` hands back exactly what a failure hands
    // back: work was done, the caller needs it, and only --no-partial-emit says
    // otherwise. Cloned rather than moved so the success arm can still consume
    // `last_success` for its own (stricter) emit choice.
    let partial_pick: Option<String> = if opts.no_partial_emit {
        None
    } else {
        let nonempty = |id: &String| {
            outputs
                .get(id)
                .map(|o| !o.output.trim().is_empty())
                .unwrap_or(false)
        };
        opts.emit
            .clone()
            .filter(nonempty)
            .or_else(|| last_success.clone().filter(nonempty))
            .or_else(|| last_executed.clone().filter(nonempty))
    };
    // Shared by meta.json's final write and the run_end event just below it:
    // the two are meant to agree exactly (P1-03's restore precedence relies
    // on that - see load_resume_for_flow), so both read it from the same
    // computation rather than two calls that could drift apart.
    let final_elapsed_sec = elapsed_before_attempt.saturating_add(flow_start.elapsed().as_secs());
    let mut meta_final = meta.clone();
    if let Some(m) = meta_final.as_object_mut() {
        m.insert("finished_utc".into(), json!(utc_stamp()));
        m.insert("leaf_runs".into(), json!(total));
        m.insert("cost_usd".into(), json!(cost_usd));
        m.insert("elapsed_sec".into(), json!(final_elapsed_sec));
        m.insert(
            "status".into(),
            json!(match &result {
                Ok(FlowEnd::Completed) => "ok",
                Ok(FlowEnd::Stuck { .. }) => "stuck",
                Err(_) => "failed",
            }),
        );
    }
    let meta_final_text = match serde_json::to_string_pretty(&meta_final) {
        Ok(text) => text,
        Err(e) => {
            let msg = format!("cannot serialize final run metadata: {e}");
            let _ = finish("failed", cost_usd, 1, None, Some(&msg));
            return Err(msg);
        }
    };
    if let Err(e) = contain::write_private_atomic(&run_dir.join("meta.json"), meta_final_text) {
        let msg = format!("cannot persist final run metadata: {e}");
        let _ = finish("failed", cost_usd, 1, None, Some(&msg));
        return Err(msg);
    }

    // Shared by the two terminals a caller can pick up again. Paths are quoted
    // (flow names may carry spaces since R-6, and so may the runs dir) and this
    // attempt's --var overrides are repeated, so the printed command works when
    // pasted back even on a resume that predates meta.json var restoration.
    let print_resume_hint = || {
        let mut var_args = String::new();
        for (k, v) in &opts.vars {
            var_args.push_str(&format!(
                " --var {}",
                execute::shell_quote(&format!("{k}={v}"))
            ));
        }
        eprintln!(
            "sfh: resume with: sfh run {}{var_args} --resume {}",
            execute::shell_quote(&opts.flow_path.display().to_string()),
            execute::shell_quote(&run_dir.display().to_string())
        );
    };
    // Whatever finished work exists, handed back the way a failure hands it
    // back - a run the caller has to act on is exactly when it is needed.
    let emit_partial = |pick: &Option<String>| {
        if opts.as_json {
            return;
        }
        if let Some(id) = pick {
            if let Some(o) = outputs.get(id) {
                if !o.output.trim().is_empty() {
                    eprintln!("sfh: emitting partial result from step '{id}'");
                    print_emit(&o.output, max_emit, chain_files.get(id));
                }
            }
        }
    };

    // The terminal workspace checkpoint, recorded before the outcome is
    // published: whatever happens to the workspace next - an automatic cleanup,
    // a human poking at it - the log already says what it held when the run
    // stopped.
    let final_state = match &result {
        Ok(FlowEnd::Completed) => "done",
        Ok(FlowEnd::Stuck { .. }) => "stuck",
        Err(_) => "failed",
    };
    if let Some(ws) = &workspace {
        if ws.mode == flow::WorkspaceMode::GitWorktree {
            match workspace::fingerprint(&ws.path) {
                Ok(fp) => log_event(
                    &mut log,
                    json!({"ts": utc_stamp(), "event": "workspace_checkpoint",
                           "workspace_id": ws.id, "at": "terminal", "fingerprint": fp}),
                )?,
                Err(e) => log_event(
                    &mut log,
                    json!({"ts": utc_stamp(), "event": "workspace_checkpoint",
                           "workspace_id": ws.id, "at": "terminal", "fingerprint": serde_json::Value::Null,
                           "error": e}),
                )?,
            }
        }
        // Cleanup can decline, and it can fail. Neither turns a successful run
        // into a failed one: the work is done and recorded, and a worktree that
        // could not be removed is a housekeeping note, not a result.
        let outcome = workspace::cleanup_auto(ws, final_state);
        if !matches!(outcome, workspace::Cleanup::NotApplicable) {
            log_event(
                &mut log,
                json!({"ts": utc_stamp(), "event": "workspace_cleanup",
                       "workspace_id": ws.id, "path": ws.path.display().to_string(),
                       "outcome": outcome.as_json()}),
            )?;
            if let workspace::Cleanup::Failed(e) = &outcome {
                if !opts.quiet && !opts.as_json {
                    eprintln!("sfh: warning: could not remove the managed workspace: {e}");
                }
            }
        }
    }

    match result {
        Ok(FlowEnd::Completed) => {
            log_event(
                &mut log,
                json!({"ts": utc_stamp(), "event": "run_end", "status": "ok", "leaf_runs": total, "cost_usd": cost_usd, "elapsed_sec": final_elapsed_sec}),
            )?;
            let emit_id = opts.emit.clone().or(last_success);
            let emit_id =
                emit_id.filter(|id| outputs.get(id).is_some_and(|output| output.exit == 0));
            if let Some(id) = &emit_id {
                let out = outputs
                    .get(id)
                    .map(|s| s.output.clone())
                    .unwrap_or_default();
                // Emit first: `sfh wait` treats a terminal status.json as "the
                // output is ready", so the status must not go terminal before it.
                // In JSON mode the result travels inside the envelope instead,
                // because stdout must hold the envelope and nothing else.
                if !opts.as_json {
                    print_emit(&out, max_emit, chain_files.get(id));
                }
            } else if last_executed.is_none() {
                // Rare, but leaving status.json on "running" would make this
                // look like a run that got killed rather than one that ended.
                finish("failed", cost_usd, 1, None, Some("no step was executed"))?;
                return Err("no step was executed".into());
            }
            finish("done", cost_usd, 0, emit_id.as_deref(), None)?;
            if opts.as_json {
                emit_run_envelope(RunEnvelope {
                    ok: true,
                    state: "done",
                    exit_code: 0,
                    run_dir: &run_dir,
                    flow: &opts.flow_path,
                    error: None,
                    result: emit_id
                        .as_deref()
                        .and_then(|id| outputs.get(id))
                        .map(|o| o.output.as_str()),
                    result_file: emit_id.as_deref().and_then(|id| chain_files.get(id)),
                    result_step: emit_id.as_deref(),
                    max_emit,
                    workspace: workspace.as_ref(),
                    leaf_runs: total,
                    cost_usd,
                });
            } else if !opts.quiet {
                eprintln!(
                    "sfh: done. {} leaf runs, ${cost_usd:.4} reported. run dir: {}",
                    total,
                    run_dir.display()
                );
            }
            Ok(0)
        }
        // The third terminal. Not a failure - nothing broke, and the run dir is
        // a coherent stopping point - but not a success either: something the
        // flow declared as needing a human was reached, and a parent agent that
        // read exit 0 would carry on as if the work were finished.
        Ok(FlowEnd::Stuck { after }) => {
            let msg = format!("routed to stuck after '{after}'");
            log_event(
                &mut log,
                json!({"ts": utc_stamp(), "event": "run_end", "status": "stuck", "error": msg, "after": after, "leaf_runs": total, "cost_usd": cost_usd, "elapsed_sec": final_elapsed_sec}),
            )?;
            if opts.as_json {
                finish("stuck", cost_usd, 4, partial_pick.as_deref(), Some(&msg))?;
                emit_run_envelope(RunEnvelope {
                    ok: false,
                    state: "stuck",
                    exit_code: 4,
                    run_dir: &run_dir,
                    flow: &opts.flow_path,
                    error: Some((machine::ErrorCode::Stuck, msg.as_str())),
                    result: partial_pick
                        .as_deref()
                        .and_then(|id| outputs.get(id))
                        .map(|o| o.output.as_str()),
                    result_file: partial_pick.as_deref().and_then(|id| chain_files.get(id)),
                    result_step: partial_pick.as_deref(),
                    max_emit,
                    workspace: workspace.as_ref(),
                    leaf_runs: total,
                    cost_usd,
                });
                return Ok(4);
            }
            eprintln!("sfh: FLOW STUCK: {msg}");
            // Emit before the status goes terminal, for the same reason the
            // success path does: `sfh wait` reads a terminal status.json as
            // "the output is ready".
            emit_partial(&partial_pick);
            finish("stuck", cost_usd, 4, partial_pick.as_deref(), Some(&msg))?;
            eprintln!("sfh: run dir: {}", run_dir.display());
            print_resume_hint();
            Ok(4)
        }
        Err(msg) => {
            log_event(
                &mut log,
                json!({"ts": utc_stamp(), "event": "run_end", "status": "failed", "error": msg, "leaf_runs": total, "cost_usd": cost_usd, "elapsed_sec": final_elapsed_sec}),
            )?;
            if opts.as_json {
                finish("failed", cost_usd, 1, partial_pick.as_deref(), Some(&msg))?;
                emit_run_envelope(RunEnvelope {
                    ok: false,
                    state: "failed",
                    exit_code: 1,
                    run_dir: &run_dir,
                    flow: &opts.flow_path,
                    error: Some((run_failure_code(&msg), msg.as_str())),
                    result: partial_pick
                        .as_deref()
                        .and_then(|id| outputs.get(id))
                        .map(|o| o.output.as_str()),
                    result_file: partial_pick.as_deref().and_then(|id| chain_files.get(id)),
                    result_step: partial_pick.as_deref(),
                    max_emit,
                    workspace: workspace.as_ref(),
                    leaf_runs: total,
                    cost_usd,
                });
                return Ok(1);
            }
            eprintln!("sfh: FLOW FAILED: {msg}");
            emit_partial(&partial_pick);
            finish("failed", cost_usd, 1, partial_pick.as_deref(), Some(&msg))?;
            eprintln!("sfh: run dir: {}", run_dir.display());
            print_resume_hint();
            Ok(1)
        }
    }
}

/// The terminal answer a `run --json` caller receives.
struct RunEnvelope<'a> {
    ok: bool,
    state: &'static str,
    exit_code: i32,
    run_dir: &'a Path,
    flow: &'a Path,
    error: Option<(machine::ErrorCode, &'a str)>,
    /// The emitted text, subject to the same `max_emit_chars` ceiling the human
    /// path applies. `result_file` always names the complete text on disk, so a
    /// caller that needs all of it never has to raise the ceiling.
    result: Option<&'a str>,
    result_file: Option<&'a PathBuf>,
    result_step: Option<&'a str>,
    max_emit: usize,
    workspace: Option<&'a workspace::Workspace>,
    leaf_runs: u32,
    cost_usd: f64,
}

/// Which stable code a flow failure maps to.
///
/// The engine's failure messages are prose that is allowed to improve, so the
/// mapping keys off the markers sfh itself writes. Anything unrecognised stays
/// a plain flow failure rather than being forced into a code that would tell a
/// caller something specific and wrong.
pub(crate) fn run_failure_code(msg: &str) -> machine::ErrorCode {
    for code in [
        machine::ErrorCode::ProtocolInvalid,
        machine::ErrorCode::TerminalMissing,
        machine::ErrorCode::SessionUnverified,
        machine::ErrorCode::ExecutionClosureChanged,
        machine::ErrorCode::WorkspaceMissing,
        machine::ErrorCode::WorkspaceDrift,
        machine::ErrorCode::WorkspaceUnowned,
        machine::ErrorCode::WorkspaceBusy,
        machine::ErrorCode::RunBusy,
        machine::ErrorCode::ReplayRefused,
        machine::ErrorCode::PersistenceFailure,
        machine::ErrorCode::CapabilityUnavailable,
        machine::ErrorCode::Stuck,
        machine::ErrorCode::Interrupted,
    ] {
        if msg.contains(code.as_str()) {
            return code;
        }
    }
    if msg.contains("did not match its documented machine-readable format")
        || msg.contains("not its documented machine-readable output")
    {
        return machine::ErrorCode::ProtocolInvalid;
    }
    if msg.contains("documented terminal record") || msg.contains("result envelope") {
        return machine::ErrorCode::TerminalMissing;
    }
    if msg.contains("resume unverified") || msg.contains("resume mismatch") {
        return machine::ErrorCode::SessionUnverified;
    }
    if msg.contains("persist") {
        return machine::ErrorCode::PersistenceFailure;
    }
    machine::ErrorCode::FlowInvalid
}

fn emit_run_envelope(e: RunEnvelope<'_>) {
    let truncated = e.result.map(|r| {
        let n = r.chars().count();
        if n > e.max_emit {
            r.chars().take(e.max_emit).collect::<String>()
        } else {
            r.to_string()
        }
    });
    let body = json!({
        "state": e.state,
        "terminal": true,
        "run_id": e.run_dir.file_name().map(|n| n.to_string_lossy().into_owned()),
        "run_dir": e.run_dir.display().to_string(),
        "flow": abs(e.flow).display().to_string(),
        "result": truncated,
        "result_step": e.result_step,
        "result_file": e.result_file.map(|p| p.display().to_string()),
        "result_truncated": e.result.map(|r| r.chars().count() > e.max_emit),
        "leaf_runs": e.leaf_runs,
        "cost_usd": e.cost_usd,
        "workspace": e.workspace.map(|w| json!({
            "path": w.path.display().to_string(),
            "mode": w.mode.as_str(),
            "branch": w.branch,
        })),
        "next_actions": next_actions_for(e.state, e.run_dir, e.flow, e.error),
    });
    match e.error {
        Some((code, msg)) => machine::emit(&machine::error_envelope(
            "run",
            code,
            msg,
            e.exit_code,
            body,
        )),
        None => machine::emit(&machine::envelope("run", e.ok, e.exit_code, body)),
    }
}

/// Whether resuming `run_dir` right now, unchanged, would walk straight back
/// into the same `stuck`: true (naming the step) only when the log's LAST
/// recorded routing decision reached "stuck" through `on_max_visits`
/// (`PositionVia::MaxVisits`).
///
/// That step's visit counter is already at the flow's declared ceiling, and
/// `--resume` restores it as-is - see the "stuck" arm of the `position`
/// handling in `load_resume_for_flow`: resetting the counter there would
/// quietly undo the limit the flow set, which is exactly the escape hatch
/// on_max_visits exists to close. So re-entering the step on resume
/// re-triggers on_max_visits immediately, without running anything or
/// spending anything new: a provable dead end, not just a likely one.
///
/// Any OTHER route to stuck - an explicit `goto: stuck` rule, an on_error
/// stuck, a budget landing - depends on what the step actually does or
/// decides when it runs again, which resuming can legitimately change (a
/// human fixed what it was stuck on, or the AI answers differently this
/// time). Only max_visits exhaustion is deterministic enough to refuse
/// resume over.
fn stuck_step_exhausted_max_visits(run_dir: &Path) -> Option<String> {
    let text = contain::read_contained_opt(run_dir, "log.jsonl")
        .ok()
        .flatten()?;
    let mut exhausted: Option<String> = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("event").and_then(|x| x.as_str()) != Some("position") {
            continue;
        }
        // Reassigned on every position event, terminal or not, so the
        // result reflects the LOG'S LAST word - a later, non-max_visits
        // position (a resumed attempt that got past it) must clear an
        // earlier max_visits landing, not leave it lingering.
        exhausted = (v.get("via").and_then(|x| x.as_str()) == Some("max_visits")).then(|| {
            v.get("after")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string()
        });
    }
    exhausted
}

/// Build one diagnosed `resume` or `carry_budget` action. `ok_field` is
/// "resumable" or "carryable" - the review's own names for the two
/// diagnoses, kept as the literal JSON key so a caller reads exactly what
/// was asked for rather than a generic "runnable" flag that does not say
/// which question it answers.
///
/// `argv` is present ONLY when `ok` is true: an unrunnable action is never
/// handed a command to run verbatim (P1-09's central rule), but it is still
/// reported - with `reason` and, when relevant, `requires` - so "nothing is
/// runnable" is something the caller is TOLD, not left to infer from a
/// shorter list.
fn diagnosed_action(
    kind: &str,
    ok_field: &str,
    ok: bool,
    reason: String,
    requires: &[&str],
    argv: Vec<String>,
) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("kind".into(), json!(kind));
    m.insert(ok_field.to_string(), json!(ok));
    m.insert("reason".into(), json!(reason));
    m.insert("requires".into(), json!(requires));
    if ok {
        m.insert("argv".into(), json!(argv));
    }
    serde_json::Value::Object(m)
}

/// Runnable follow-ups, as argv rather than prose, so a caller can act
/// without parsing an instruction - "why" (and, mid-run, "wait") always
/// qualify, but `resume` and `carry_budget` do not just because the state is
/// `failed` or `stuck`: a persistence failure is not resumable, a stuck run
/// that exhausted max_visits sticks again the instant it is resumed, and a
/// workspace or execution-closure problem needs a specific flag before
/// resuming can work at all (P1-09). Each is diagnosed here rather than
/// assumed from the state alone, using the same durable facts a human would
/// have to go check by hand: the classified failure code already computed
/// for this envelope's `error`, the log's own last routing decision, and -
/// for carry - the exact finality proof `--carry-budget-from` itself
/// requires (`carry_source_is_final`, P1-02). An action this run cannot
/// actually complete is never handed a command to run; only the diagnosis
/// that explains why is.
fn next_actions_for(
    state: &str,
    run_dir: &Path,
    flow: &Path,
    error: Option<(machine::ErrorCode, &str)>,
) -> Vec<serde_json::Value> {
    let rd = run_dir.display().to_string();
    let mut out = vec![machine::next_action(
        "why",
        vec![
            "sfh".into(),
            "runs".into(),
            "why".into(),
            rd.clone(),
            "--json".into(),
        ],
    )];
    match state {
        "running" => out.insert(
            0,
            machine::next_action(
                "wait",
                vec!["sfh".into(), "wait".into(), rd.clone(), "--json".into()],
            ),
        ),
        "failed" | "stuck" => {
            let (resumable, resume_reason, requires): (bool, String, Vec<&str>) = if state
                == "stuck"
            {
                match stuck_step_exhausted_max_visits(run_dir) {
                    Some(step) => (
                        false,
                        format!(
                            "step '{step}' is stuck because it reached its declared max_visits; resuming walks back into the same step and sticks again immediately, without running anything. Raise max_visits (or change on_max_visits) in the flow, then resume."
                        ),
                        vec![],
                    ),
                    None => (
                        true,
                        "a human decision was requested; resuming continues from where the flow left off.".to_string(),
                        vec![],
                    ),
                }
            } else {
                match error.map(|(c, _)| c) {
                    Some(machine::ErrorCode::PersistenceFailure) => (
                        false,
                        "a required result could not be durably persisted, so this run is not resumable: replaying it risks re-running an effect whose completion cannot be verified. Verify the external side effect by hand, then start a fresh run.".to_string(),
                        vec![],
                    ),
                    Some(machine::ErrorCode::WorkspaceMissing) => (
                        false,
                        "the managed workspace this run recorded no longer exists, so there is nothing here to resume into. Restore it, or start a fresh run.".to_string(),
                        vec![],
                    ),
                    Some(machine::ErrorCode::WorkspaceDrift) => (
                        true,
                        "the managed workspace changed outside this run since its last checkpoint; --adopt-workspace accepts the current contents as the new baseline.".to_string(),
                        vec!["--adopt-workspace"],
                    ),
                    Some(machine::ErrorCode::ExecutionClosureChanged) => (
                        true,
                        "the pinned execution inputs (profile, model, context or tool) changed since this run started; --force-resume accepts the change and continues.".to_string(),
                        vec!["--force-resume"],
                    ),
                    _ => (
                        true,
                        "the run stopped before completing; resuming continues from the last durable checkpoint.".to_string(),
                        vec![],
                    ),
                }
            };
            let mut resume_argv = vec![
                "sfh".to_string(),
                "run".into(),
                flow.display().to_string(),
                "--resume".into(),
                rd.clone(),
            ];
            resume_argv.extend(requires.iter().map(|f| f.to_string()));
            resume_argv.push("--json".into());
            out.push(diagnosed_action(
                "resume",
                "resumable",
                resumable,
                resume_reason,
                &requires,
                resume_argv,
            ));

            // Offered alongside resume, not instead of it, because the two
            // are answers to different diagnoses and only the reader knows
            // which one they are looking at. A caller who concludes the FLOW
            // was wrong cannot resume - correcting it invalidates the
            // closure, as it should - and without this the only remaining
            // move is a fresh run whose budget silently starts over. It
            // depends on a DIFFERENT fact than resume does - not "can this
            // run continue" but "is this run's spend final" - so it is
            // diagnosed separately, via the same proof `--carry-budget-from`
            // itself requires (P1-02). At the point this function is called
            // that proof always succeeds (this run just wrote its own
            // terminal status.json a few lines up), but it is still asked
            // for rather than assumed: the two questions being answered by
            // the same code, here and at the real carry attempt, is the
            // point.
            let carryable = carry_source_is_final(run_dir).is_ok();
            let carry_reason = if carryable {
                "this run's spend is final; a corrected flow can start fresh while keeping it on the books instead of silently resetting the budget.".to_string()
            } else {
                "this run's spend cannot yet be confirmed final, so carrying from it would risk a snapshot the source immediately invalidates.".to_string()
            };
            out.push(diagnosed_action(
                "carry_budget",
                "carryable",
                carryable,
                carry_reason,
                &[],
                vec![
                    "sfh".into(),
                    "run".into(),
                    flow.display().to_string(),
                    "--carry-budget-from".into(),
                    rd,
                    "--json".into(),
                ],
            ));
        }
        _ => {}
    }
    out
}

/// `chain` must be the file the emitted step's LAST visit wrote, not a path
/// rebuilt from the step id - those differ once a step is re-visited.
fn print_emit(out: &str, max: usize, chain: Option<&PathBuf>) {
    let n = out.chars().count();
    if n > max {
        let cut: String = out.chars().take(max).collect();
        println!(
            "{cut}\n[sfh: emit truncated at {max} of {n} chars; full text: {}]",
            chain
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(run dir)".into())
        );
        eprintln!("sfh: warning: emit truncated at max_emit_chars={max}");
    } else {
        println!("{out}");
        if n > 20_000 {
            eprintln!(
                "sfh: warning: emitted {n} chars to stdout - consider a summarizing final step or defaults.max_emit_chars"
            );
        }
    }
}

fn resume_target(opts: &RunOpts, runs_root: &Path) -> Result<Option<PathBuf>, String> {
    if let Some(d) = &opts.resume {
        match contain::read_contained_opt(d, "log.jsonl") {
            Ok(Some(_)) => {}
            Ok(None) => return Err(format!("{} is not an sfh run directory", d.display())),
            Err(e) => {
                return Err(format!(
                    "{} is not a safe sfh run directory: {e}",
                    d.display()
                ))
            }
        }
        return Ok(Some(d.clone()));
    }
    if opts.resume_latest {
        return latest_run_dir(runs_root, &opts.flow_path)
            .map(Some)
            .ok_or_else(|| {
                format!(
                    "no previous run of {} found under {}",
                    opts.flow_path.display(),
                    runs_root.display()
                )
            });
    }
    Ok(None)
}

fn next_label(idx: usize, flow: &flow::Flow) -> String {
    flow.steps
        .get(idx)
        .map(|s| s.id.clone())
        .unwrap_or_else(|| "end".into())
}

fn write_aggregate(
    run_dir: &Path,
    gtag: &str,
    agg: &str,
    outputs: &mut BTreeMap<String, template::StepOutput>,
    step_id: &str,
    failed: bool,
) -> Result<(), String> {
    let gfile = run_dir.join(format!("{gtag}.out.txt"));
    contain::write_private_atomic(&gfile, agg)
        .map_err(|e| format!("cannot persist aggregate {}: {e}", gfile.display()))?;
    let chain = run_dir.join(format!("{gtag}.chain.txt"));
    contain::write_private_atomic(&chain, agg)
        .map_err(|e| format!("cannot persist aggregate {}: {e}", chain.display()))?;
    outputs.insert(
        step_id.to_string(),
        template::StepOutput {
            output: agg.to_string(),
            outputs: agg.to_string(),
            output_file: gfile.display().to_string(),
            exit: if failed { 1 } else { 0 },
            stderr_file: String::new(),
            // A fan-out group runs no command of its own, so it has no exit
            // code for an `outcomes:` table to describe.
            outcome: String::new(),
            label: String::new(),
        },
    );
    Ok(())
}

/// The outcome class an `outcomes:` entry gave a finished leaf, as text.
///
/// Empty when the flow declared nothing for the code the step exited with,
/// which is the ordinary case and the reason routing on it fails closed rather
/// than matching a default.
fn outcome_name(d: &leaf::LeafDone) -> String {
    d.outcome
        .as_ref()
        .map(|(r, _)| r.as_str().to_string())
        .unwrap_or_default()
}

/// The free-form label from that same entry. sfh stores and compares it and
/// never interprets it.
fn outcome_label(d: &leaf::LeafDone) -> String {
    d.outcome
        .as_ref()
        .and_then(|(_, l)| l.clone())
        .unwrap_or_default()
}

fn note_marker(step: &str, visit: u32, output: &str) -> String {
    // Content-derived instead of attempt-nonce-derived: sfh intentionally
    // rotates the stop nonce on every --resume, while this marker must remain
    // stable across attempts. Including the exact note body makes accidental
    // collisions with model output impractical.
    let digest = fingerprint(&format!("{step}\0{visit}\0{output}"));
    format!("<!-- sfh-note:{} -->", &digest[..24])
}

fn write_note_once(
    run_dir: &Path,
    notes_file: &Path,
    marker: &str,
    step: &str,
    visit: u32,
    output: &str,
) -> Result<(), String> {
    let mut notes = contain::read_contained_opt(run_dir, "notes.md")?.unwrap_or_default();
    if notes.lines().any(|line| line.trim() == marker) {
        return Ok(());
    }
    if !notes.is_empty() && !notes.ends_with('\n') {
        notes.push('\n');
    }
    notes.push_str(marker);
    notes.push('\n');
    notes.push_str(&format!("## {step} (visit {visit})\n"));
    notes.push_str(output.trim_end());
    notes.push_str("\n\n");
    contain::write_private_atomic(notes_file, notes).map_err(|e| {
        format!(
            "cannot atomically persist notes {}: {e}",
            notes_file.display()
        )
    })
}

fn head_tail(s: &str, budget: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= budget {
        return s.to_string();
    }
    // Keep both ends: trailing verdict markers are the shipped convention.
    let half = (budget / 2).max(1);
    let head: String = chars[..half].iter().collect();
    let tail: String = chars[chars.len() - half..].iter().collect();
    format!("{head}\n...[sfh: truncated middle]...\n{tail}")
}

fn foreach_items_hash(items: &[String]) -> String {
    // Length-prefix every UTF-8 item, including the count, so neither embedded
    // delimiters nor ["ab", "c"] versus ["a", "bc"] can collide at the input
    // encoding layer. `fingerprint` then supplies the public SHA-256 digest.
    let mut material = format!("{}:", items.len());
    for item in items {
        material.push_str(&item.len().to_string());
        material.push(':');
        material.push_str(item);
    }
    fingerprint(&material)
}

struct CompactRun<'a, 'ctx> {
    flow: &'a flow::Flow,
    ctx: &'a template::Ctx<'ctx>,
    original: &'a str,
    run_dir: &'a Path,
    tag: &'a str,
    /// The summarizer is a child of this run like any other, so its output
    /// counts as the run being alive.
    run_clock: &'a Arc<AtomicU64>,
    wall_deadline: Option<Instant>,
    /// The summarizer runs where the rest of the run runs, so a compaction that
    /// shells out sees the same working tree its step did.
    workspace: Option<&'a Path>,
    quiet: bool,
    verbose: bool,
}

struct CompactOutcome {
    summary: Result<String, String>,
    /// Always returned once an external summarizer was launched, including
    /// non-zero exit, timeout, empty output, or artifact-persistence failure.
    /// Dropping failed-attempt usage would let compact work escape the run's
    /// accounting and max_cost_usd guard.
    usage: preset::Usage,
}

fn run_compact(comp: &flow::Compact, run: CompactRun<'_, '_>) -> Result<CompactOutcome, String> {
    let prep = prepare_compact(comp, &run)?;
    let d = leaf::exec_leaf(prep);
    let summary = if let Some(e) = &d.persistence_error {
        Err(e.clone())
    } else if !d.ok() {
        Err(format!(
            "summarizer exit={} timed_out={}",
            d.exit_code, d.timed_out
        ))
    } else {
        let s = d.chain_output.trim().to_string();
        if s.is_empty() {
            Err("summarizer returned empty output".into())
        } else {
            Ok(s)
        }
    };
    Ok(CompactOutcome {
        summary,
        usage: d.usage,
    })
}

fn prepare_compact(
    comp: &flow::Compact,
    run: &CompactRun<'_, '_>,
) -> Result<leaf::Prepared, String> {
    let CompactRun {
        flow,
        ctx,
        original,
        run_dir,
        tag,
        run_clock,
        wall_deadline,
        workspace: _,
        quiet,
        verbose,
    } = run;
    let prof = comp.use_.as_ref().and_then(|u| flow.profiles.get(u));
    let tool = comp
        .tool
        .clone()
        .or_else(|| prof.and_then(|p| p.tool.clone()))
        .ok_or("compact: no tool resolved")?;
    let bin = comp
        .bin
        .clone()
        .or_else(|| prof.and_then(|p| p.bin.clone()));
    let model = comp
        .model
        .clone()
        .or_else(|| prof.and_then(|p| p.model.clone()));
    let effort = comp
        .effort
        .clone()
        .or_else(|| prof.and_then(|p| p.effort.clone()));
    let target = comp.target_chars.unwrap_or(comp.when_over / 2).max(200);
    // Never ship an unbounded blob to the summarizer - the feature exists to
    // save money, not to spend it.
    let cap = comp.max_input_chars.unwrap_or(120_000) as usize;
    let body = head_tail(original, cap);
    let instr = match &comp.instruction {
        Some(instruction) => template::render(instruction, ctx)
            .map_err(|e| format!("compact.instruction: {e}"))?,
        None => format!(
            "Summarize the text below in at most {target} characters, in the same language as the text. \
             It will be passed to another AI agent as context, so keep every conclusion, number, file path and open question. \
             Output only the summary."
        ),
    };
    let prompt = format!("{instr}\n\n---\n{body}");
    let ctag = format!("{tag}.compact");
    let prompt_file = run_dir.join(format!("{ctag}.prompt.txt"));
    contain::write_private(&prompt_file, &prompt).map_err(|e| e.to_string())?;
    let last = run_dir.join(format!("{ctag}.last.txt"));
    let paths = preset::BuildPaths {
        last_msg: &last,
        prompt_file: &prompt_file,
    };
    let timeout_sec = comp.timeout_sec.or(Some(600));
    let inp = preset::PresetInput {
        model,
        effort,
        access: preset::Access::Read,
        agent: None,
        extra: &[],
        bin,
        timeout_sec,
    };
    let built = preset::build(&tool, inp, &paths, None)?;
    let mut argv = built.argv;
    let stdin_payload = match built.delivery {
        preset::Delivery::Stdin => Some(prompt.clone().into_bytes()),
        preset::Delivery::PromptFile => None,
        preset::Delivery::Arg => {
            argv.push(prompt.clone());
            None
        }
        preset::Delivery::None => None,
    };
    Ok(leaf::Prepared {
        tag: ctag.clone(),
        inv: execute::Invocation::Argv(argv),
        parse: built.parse,
        stdin_payload,
        // A compact summarizer only reads the text it was handed. It still runs
        // in the run's workspace so a `cmd`-shaped summarizer sees the same
        // tree, but it names no context of its own.
        context_hash: None,
        context_file: None,
        cwd: run.workspace.map(PathBuf::from),
        timeout: timeout_sec.map(Duration::from_secs),
        wall_deadline: *wall_deadline,
        retry_landing_deadline: None,
        preassigned_session: None,
        expect_session: None,
        expect_marker: None,
        forbid_session: None,
        expect_parent: None,
        warmup_key: None,
        env_remove: built.env_remove,
        env_set: built.env_set,
        run_dir: run_dir.to_path_buf(),
        out_file: run_dir.join(format!("{ctag}.out.txt")),
        err_file: run_dir.join(format!("{ctag}.err.txt")),
        chain_file: run_dir.join(format!("{ctag}.chain.txt")),
        tool: Some(tool),
        access: Some(preset::Access::Read),
        session_parent: None,
        allow_empty: false,
        // A step's `exit_conflict:` is a statement about the tool that step
        // launches. The summarizer may be a different tool entirely, so it
        // keeps its own adapter default rather than inheriting a licence the
        // flow granted to something else.
        exit_conflict: None,
        // Same reasoning: the step's exit-code table describes the command that
        // step runs, not the summarizer sfh launches on its behalf.
        outcomes: BTreeMap::new(),
        retry: leaf::RetryCfg::default(),
        run_clock: Some(Arc::clone(run_clock)),
        quiet: *quiet,
        verbose: *verbose,
    })
}

/// Balanced top-level bracket spans in prose, in source order. Brackets inside
/// JSON strings and nested arrays stay inside the outer candidate. This is
/// deliberately a small extractor rather than a permissive JSON parser: every
/// candidate still has to parse as an actual JSON array before it can fan out.
fn json_array_spans(text: &str) -> Vec<&str> {
    let mut spans = Vec::new();
    let mut start = None;
    let mut depth = 0u32;
    let mut in_string = false;
    let mut escaped = false;

    for (index, ch) in text.char_indices() {
        if depth == 0 {
            if ch == '[' {
                start = Some(index);
                depth = 1;
                in_string = false;
                escaped = false;
            }
            continue;
        }

        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '[' => depth = depth.saturating_add(1),
            ']' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(begin) = start.take() {
                        spans.push(&text[begin..index + ch.len_utf8()]);
                    }
                }
            }
            _ => {}
        }
    }
    spans
}

fn split_items(s: &str, mode: Option<&str>) -> Result<Vec<String>, String> {
    match mode.unwrap_or("lines") {
        "lines" => Ok(s
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()),
        "json" => {
            let t = s.trim();
            // Tolerate code fences / prose around the array.
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(t);
            let v = match parsed {
                Ok(v) => v,
                Err(whole_error) => {
                    // AI output often puts a citation such as `[1]` before its
                    // final array. Taking first `[` through last `]` joins the
                    // citation and array into invalid JSON. Walk complete
                    // balanced candidates from the end and use the last one
                    // that is genuinely an array; "final structured answer
                    // wins" is deterministic and keeps nested arrays intact.
                    let spans = json_array_spans(t);
                    let mut candidate_error = None;
                    let mut selected = None;
                    for candidate in spans.iter().rev() {
                        match serde_json::from_str::<serde_json::Value>(candidate) {
                            Ok(value) if value.is_array() => {
                                selected = Some(value);
                                break;
                            }
                            Ok(_) => {}
                            Err(error) => candidate_error = Some(error),
                        }
                    }
                    selected.ok_or_else(|| {
                        if spans.is_empty() {
                            "foreach: no JSON array found in input".to_string()
                        } else {
                            format!(
                                "foreach: invalid JSON array: {}",
                                candidate_error.unwrap_or(whole_error)
                            )
                        }
                    })?
                }
            };
            let arr = v.as_array().ok_or("foreach: JSON must be an array")?;
            Ok(arr
                .iter()
                .map(|x| match x {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect())
        }
        m if m.starts_with("separator:") => {
            let sep = &m["separator:".len()..];
            if sep.is_empty() {
                return Err("foreach: empty separator".into());
            }
            Ok(s.split(sep)
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect())
        }
        other => Err(format!("foreach: bad split '{other}'")),
    }
}

fn one_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("");
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max || s.lines().count() > 1 {
        out.push_str("...");
    }
    out
}

/// What `plan` needs beyond the rendering itself: the structural decisions this
/// flow would make, which is most of what a reviewer is actually checking.
pub struct DryRunExtras<'a> {
    pub flow_path: &'a Path,
    pub workspace: &'a flow::WorkspacePlan,
    pub state_root: &'a state::StateRoot,
    pub profiles: &'a [PathBuf],
    pub as_json: bool,
    /// `plan --save`: where to keep the rendered prompts, context bundles and
    /// machine plan so a human can read exactly what would be sent before
    /// anyone pays for it. `Some(None)` means "save, and pick the default
    /// location under the state root".
    pub save: Option<Option<PathBuf>>,
}

impl DryRunExtras<'_> {
    /// A short label for the saved plan directory.
    fn workspace_name(&self) -> &str {
        self.flow_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("plan")
    }
}

#[allow(clippy::too_many_arguments)]
fn dry_run(
    flow: &flow::Flow,
    vars: &BTreeMap<String, String>,
    tainted_vars: &HashSet<String>,
    run_dir: &Path,
    flow_dir: &Path,
    notes_file: &Path,
    needed_sessions: &HashSet<String>,
    extras: DryRunExtras,
) -> Result<i32, String> {
    let step_ids = flow.step_ids();
    // `plan` has no real upstream results, but rendering them as empty made a
    // perfectly valid downstream `stdin: prompt` look like an empty prompt and
    // aborted the plan. Stable, visibly synthetic values let every dominated
    // dependency resolve without pretending an external command ran.
    let outputs: BTreeMap<String, template::StepOutput> = step_ids
        .iter()
        .map(|id| {
            (
                id.clone(),
                template::StepOutput {
                    output: format!("[[steps.{id}.output]]"),
                    outputs: format!("[[steps.{id}.outputs]]"),
                    output_file: format!("[[steps.{id}.output_file]]"),
                    exit: 0,
                    stderr_file: format!("[[steps.{id}.stderr_file]]"),
                    outcome: format!("[[steps.{id}.outcome]]"),
                    label: format!("[[steps.{id}.label]]"),
                },
            )
        })
        .collect();
    contain::write_private(notes_file, "[[notes]]")
        .map_err(|e| format!("cannot prepare the isolated plan notes placeholder: {e}"))?;
    let mut sessions: HashMap<String, leaf::SessionInfo> = HashMap::new();
    // Fake sessions so continue_from/fork_from steps can render their resume
    // command. The target's own access comes along, so a dry run already shows
    // the escalation guard refusing a higher-access resume.
    for s in &flow.steps {
        let mut targets: Vec<&String> = Vec::new();
        for st in std::iter::once(s).chain(s.parallel.iter().flat_map(|cs| cs.iter())) {
            for t in [&st.continue_from, &st.fork_from].into_iter().flatten() {
                targets.push(t);
            }
        }
        for t in targets {
            if let Some(target_step) = flow.find_step(t) {
                if let Ok(eff) = leaf::effective(flow, target_step) {
                    if let Some(tool) = eff.tool {
                        sessions.insert(
                            t.clone(),
                            leaf::SessionInfo {
                                tool,
                                id: "<session-id>".into(),
                                cwd: None,
                                marker: None,
                                access: Some(eff.access),
                                access_recorded: true,
                            },
                        );
                    }
                }
            }
        }
    }
    // In JSON mode stdout carries the envelope and nothing else, so the header
    // a human wants goes to stderr instead of being printed and then having to
    // be stripped by every caller.
    if extras.as_json {
        return dry_run_json(
            flow,
            vars,
            tainted_vars,
            run_dir,
            flow_dir,
            notes_file,
            needed_sessions,
            &extras,
            &outputs,
            &sessions,
            &step_ids,
        );
    }
    println!("execution plan: commands are resolved but no process is started");
    println!(
        "temporary render dir (removed after this command): {}",
        run_dir.display()
    );
    println!("upstream results are shown as [[steps.<id>.*]] placeholders (exit uses 0)");
    for warning in flow::strict_warnings(flow) {
        println!("warning: {warning}");
    }
    for warning in flow.replay_warnings() {
        println!("warning: {warning}");
    }
    println!(
        "workspace: {}",
        serde_json::to_string(&extras.workspace.to_json()).unwrap_or_default()
    );
    for tool in flow
        .resolved_tools()
        .into_iter()
        .filter(|tool| tool.require_version.is_some())
    {
        let program = tool
            .bin
            .clone()
            .unwrap_or_else(|| preset::default_program(&tool.tool));
        println!(
            "required version: {} ({}) {}",
            tool.tool,
            program,
            tool.require_version.as_deref().unwrap_or_default()
        );
    }
    // The one goto that never appears in any step's route:, so a reader of the
    // flow cannot see it by following the steps. Print it where the jump is
    // declared - at the top, with the flow, not against any step.
    if let Some(target) = flow.defaults.budget_goto() {
        let mut reserves = Vec::new();
        if flow.defaults.max_cost_usd.is_some() {
            reserves.push(format!(
                "cost reserve ${:.2}",
                flow.defaults.budget_reserve_usd()
            ));
        }
        if flow.defaults.wall_clock_sec.is_some() {
            reserves.push(format!(
                "wall reserve {}s",
                flow.defaults.budget_reserve_sec()
            ));
        }
        println!("budget landing: goto {target} ({})", reserves.join(", "));
    }
    println!();
    let cx = leaf::PrepCtx {
        flow,
        vars,
        outputs: &outputs,
        step_ids: &step_ids,
        run_dir,
        flow_dir,
        notes_file,
        sessions: &sessions,
        needed_sessions,
        tainted_vars,
        // A dry run renders and prints; it spawns nothing to time.
        run_clock: None,
        wall_deadline: None,
        retry_landing_deadline: None,
        // Nothing has been spent and no time has passed, so {{budget.*}} shows
        // the whole declared budget - which is what a prompt reviewer wants to
        // see, and it exercises the `unlimited` spelling for undeclared axes.
        budget: leaf::BudgetVars::new(&flow.defaults, 0.0, 0),
        // A plan renders what the flow WOULD do; it never creates a workspace,
        // so leaves render against the caller's cwd exactly as `plan` always
        // has. The workspace the run would use is reported separately above.
        workspace: None,
        quiet: true,
        verbose: false,
    };
    let plan_clock = Arc::new(AtomicU64::new(0));
    for s in &flow.steps {
        println!("[{}] ({})", s.id, describe_kind(flow, s));
        if let Some(children) = &s.parallel {
            for c in children {
                let p = leaf::prepare_leaf(&cx, c, 1, &c.id, &[], None)?;
                println!("  child {}: {}", c.id, p.inv.describe());
                for fb in &c.fallback {
                    let p =
                        leaf::prepare_leaf(&cx, c, 1, &format!("{}.fb-{fb}", c.id), &[], Some(fb))?;
                    println!("    fallback {fb}: {}", p.inv.describe());
                }
            }
        } else if let Some(fe) = &s.foreach {
            println!("  foreach.from: {}", one_line(&fe.from, 100));
            println!("  split: {}", fe.split.as_deref().unwrap_or("lines"));
            let p = leaf::prepare_leaf(
                &cx,
                s,
                1,
                &format!("{}.i0", s.id),
                &[
                    ("item", "<item>".to_string()),
                    ("item_index", "0".to_string()),
                ],
                None,
            )?;
            println!("  cmd (per item): {}", p.inv.describe());
            for fb in &s.fallback {
                let p = leaf::prepare_leaf(
                    &cx,
                    s,
                    1,
                    &format!("{}.i0.fb-{fb}", s.id),
                    &[
                        ("item", "<item>".to_string()),
                        ("item_index", "0".to_string()),
                    ],
                    Some(fb),
                )?;
                println!("    fallback {fb} (per item): {}", p.inv.describe());
            }
        } else {
            let p = leaf::prepare_leaf(&cx, s, 1, &s.id, &[], None)?;
            println!("  cmd: {}", p.inv.describe());
            if let Some(t) = &s.continue_from {
                println!("  (resumes session of '{t}')");
            }
            for fb in &s.fallback {
                let p = leaf::prepare_leaf(&cx, s, 1, &format!("{}.fb-{fb}", s.id), &[], Some(fb))?;
                println!("  fallback {fb}: {}", p.inv.describe());
            }
        }
        if let Some(comp) = &s.compact {
            let prompt_file = run_dir.join(format!("{}.prompt.txt", s.id));
            let builtins = leaf::make_builtins(&cx, &s.id, 1, &prompt_file, &[]);
            let compact_ctx = template::Ctx {
                vars,
                outputs: &outputs,
                step_ids: &step_ids,
                builtins,
            };
            let run = CompactRun {
                flow,
                ctx: &compact_ctx,
                original: &format!("[[steps.{}.outputs]]", s.id),
                run_dir,
                tag: &s.id,
                run_clock: &plan_clock,
                wall_deadline: None,
                workspace: None,
                quiet: true,
                verbose: false,
            };
            let p = prepare_compact(comp, &run)?;
            println!(
                "  compact when output > {} chars: {}",
                comp.when_over,
                p.inv.describe()
            );
        }
        for r in &s.route {
            let mut cond = Vec::new();
            if let Some(c) = &r.when_contains {
                cond.push(format!("contains {c:?}"));
            }
            if let Some(c) = &r.when_matches {
                cond.push(format!("matches {c:?}"));
            }
            if let Some(c) = &r.when_last_line_contains {
                cond.push(format!("last line contains {c:?}"));
            }
            if let Some(c) = &r.when_last_line_is {
                cond.push(format!("last line is {c:?}"));
            }
            if let Some(c) = &r.when_last_line_matches {
                cond.push(format!("last line matches {c:?}"));
            }
            if let Some(c) = &r.when_exit {
                cond.push(format!("exit is {c}"));
            }
            if let Some(c) = &r.when_stderr_matches {
                cond.push(format!("stderr matches {c:?}"));
            }
            // A predicate missing from this list prints as "always", which is
            // not a cosmetic slip: the plan then shows two unconditional rules
            // on one step - a shape `validate` refuses - and the reader cannot
            // see which branch the flow will actually take.
            if let Some(c) = &r.when_label_is {
                cond.push(format!("label is {c:?}"));
            }
            if let Some(c) = &r.when_outcome_is {
                cond.push(format!("outcome is {}", c.as_str()));
            }
            if let Some(c) = r.when_protocol_is {
                cond.push(format!("protocol is {}", c.as_str()));
            }
            if let Some(m) = &r.when_members {
                let quantifier = match (m.all, m.at_least) {
                    (Some(true), _) => "all".to_string(),
                    (_, Some(n)) => format!("at least {n}"),
                    _ => "no".to_string(),
                };
                cond.push(format!("{quantifier} members end on {:?}", m.last_line_is));
            }
            let cond = if cond.is_empty() {
                "always".to_string()
            } else {
                cond.join(" && ")
            };
            println!("  route: {cond} -> {}", r.goto);
        }
        println!();
    }
    if let Some(explicit) = &extras.save {
        let dir = save_plan(run_dir, explicit.as_deref(), &extras)?;
        println!("plan saved to {}", dir.display());
    }
    Ok(0)
}

/// Copy a plan's rendered artifacts out of the temporary render directory.
///
/// That directory is removed the moment `plan` returns, so `--save` is what
/// turns a plan into something a human can actually read line by line - the
/// full prompts, the assembled context bundles and their manifests, and the
/// machine plan - and then approve, or not.
fn save_plan(
    render_dir: &Path,
    explicit: Option<&Path>,
    extras: &DryRunExtras,
) -> Result<PathBuf, String> {
    let dir = match explicit {
        Some(d) => d.to_path_buf(),
        None => extras.state_root.plans_dir().ok_or_else(|| {
            "plan --save needs a directory, or a --state-dir (SFH_STATE_DIR) to put one under"
                .to_string()
        })?,
    };
    let dir = dir.join(format!(
        "{}-{}",
        utc_stamp(),
        crate::workspace::sanitize(extras.workspace_name())
    ));
    contain::mkdir_private(&dir)
        .map_err(|e| format!("cannot create the plan directory {}: {e}", dir.display()))?;
    let entries = std::fs::read_dir(render_dir)
        .map_err(|e| format!("cannot read the render dir {}: {e}", render_dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        // Regular files only, and no recursion: everything a plan renders is a
        // flat artifact sfh itself just wrote.
        if !path.is_file() {
            continue;
        }
        if let Some(name) = path.file_name() {
            std::fs::copy(&path, dir.join(name))
                .map_err(|e| format!("cannot save {}: {e}", path.display()))?;
        }
    }
    Ok(dir)
}

/// `plan --json`: the same resolution the human plan does, as one envelope.
///
/// Still spawns nothing and still creates no workspace. Every invocation it
/// reports is the REDACTED form, so a plan can be pasted into an issue without
/// carrying the prompt - which is exactly the text most likely to be sensitive.
#[allow(clippy::too_many_arguments)]
fn dry_run_json(
    flow: &flow::Flow,
    vars: &BTreeMap<String, String>,
    tainted_vars: &HashSet<String>,
    run_dir: &Path,
    flow_dir: &Path,
    notes_file: &Path,
    needed_sessions: &HashSet<String>,
    extras: &DryRunExtras,
    outputs: &BTreeMap<String, template::StepOutput>,
    sessions: &HashMap<String, leaf::SessionInfo>,
    step_ids: &HashSet<String>,
) -> Result<i32, String> {
    let cx = leaf::PrepCtx {
        flow,
        vars,
        outputs,
        step_ids,
        run_dir,
        flow_dir,
        notes_file,
        sessions,
        needed_sessions,
        tainted_vars,
        run_clock: None,
        wall_deadline: None,
        retry_landing_deadline: None,
        budget: leaf::BudgetVars::new(&flow.defaults, 0.0, 0),
        workspace: None,
        quiet: true,
        verbose: false,
    };
    let mut steps = Vec::new();
    for s in &flow.steps {
        let retry = leaf::retry_cfg(flow, s);
        let mut invocations = Vec::new();
        let mut push = |label: String, p: &leaf::Prepared| {
            let invocation_retry = p.retry;
            invocations.push(json!({
                "label": label,
                "cmd": p.inv.describe(),
                "tool": p.tool,
                "access": p.access.map(|a| a.as_str()),
                "protocol": protocol::expected_kind(&p.parse),
                "cwd": p.cwd.as_ref().map(|c| c.display().to_string()),
                "context_hash": p.context_hash,
                "retry": {
                    "mode": invocation_retry.mode_name(),
                    "max_retries": invocation_retry.max,
                    "max_attempts": invocation_retry.max_attempts(),
                    "backoff_sec": invocation_retry.backoff_sec,
                    "counts_toward_max_total_steps": false,
                },
            }));
        };
        match (&s.parallel, &s.foreach) {
            (Some(children), _) => {
                for c in children {
                    let p = leaf::prepare_leaf(&cx, c, 1, &c.id, &[], None)?;
                    push(format!("member:{}", c.id), &p);
                }
            }
            (None, Some(_)) => {
                let p = leaf::prepare_leaf(
                    &cx,
                    s,
                    1,
                    &format!("{}.i0", s.id),
                    &[
                        ("item", "<item>".to_string()),
                        ("item_index", "0".to_string()),
                    ],
                    None,
                )?;
                push("per-item".into(), &p);
            }
            _ => {
                let p = leaf::prepare_leaf(&cx, s, 1, &s.id, &[], None)?;
                push("main".into(), &p);
            }
        }
        for fb in &s.fallback {
            let p = leaf::prepare_leaf(&cx, s, 1, &format!("{}.fb-{fb}", s.id), &[], Some(fb))?;
            push(format!("fallback:{fb}"), &p);
        }
        steps.push(json!({
            "step": s.id,
            "kind": describe_kind(flow, s),
            "effects": s.effects(flow).as_str(),
            "replay_unfinished": s.replay_policy(flow).as_str(),
            "context": s.context,
            "context_delivery": s.context_delivery().as_str(),
            "retry": {
                "mode": retry.mode_name(),
                "max_retries": retry.max,
                "max_attempts": retry.max_attempts(),
                "backoff_sec": retry.backoff_sec,
                "counts_toward_max_total_steps": false,
            },
            "invocations": invocations,
            "route": s.route.iter().map(|r| json!({"goto": r.goto})).collect::<Vec<_>>(),
        }));
    }
    // The closure a real run would pin, built the same way, so `plan --json`
    // answers "would resuming this run still be the same run" before there is
    // a run at all.
    let cl = build_closure(
        flow,
        extras.flow_path,
        extras.profiles,
        extras.workspace,
        None,
    )?;
    let mut warnings = flow::strict_warnings(flow);
    warnings.extend(flow.replay_warnings());
    warnings.extend(extras.workspace.warnings.iter().cloned());
    let body = json!({
        "flow": abs(extras.flow_path).display().to_string(),
        "flow_name": flow.name,
        "state_dir": extras.state_root.explicit().map(|p| p.display().to_string()),
        "runs_dir": extras.state_root.runs_dir().display().to_string(),
        "profile_overlays": extras.profiles.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "workspace": extras.workspace.to_json(),
        "contexts": flow.context_plan(extras.flow_path)?.to_json(),
        "execution_closure": cl.to_json(),
        "replay": flow.replay_summary(),
        "unsafe_overrides": flow.unsafe_overrides(),
        "static_max_leaves": flow.static_max_leaves(),
        "required_versions": flow.resolved_tools().into_iter().filter_map(|tool| {
            tool.require_version.map(|requirement| json!({
                "tool": tool.tool,
                "bin": tool.bin.unwrap_or_else(|| preset::default_program(&tool.tool)),
                "requirement": requirement,
                "observed": null,
            }))
        }).collect::<Vec<_>>(),
        "vars": vars,
        "steps": steps,
        "warnings": warnings,
    });
    let saved = match &extras.save {
        Some(explicit) => Some(save_plan(run_dir, explicit.as_deref(), extras)?),
        None => None,
    };
    let mut body = body;
    if let Some(map) = body.as_object_mut() {
        map.insert(
            "saved_to".into(),
            json!(saved.as_ref().map(|p| p.display().to_string())),
        );
    }
    machine::emit(&machine::envelope("plan", true, 0, body));
    Ok(0)
}

/// Everything outside the flow file that decides what this run does.
///
/// `tool_versions` is passed in when a real run has already probed them, and
/// omitted by `plan`, which must not spawn anything: a plan therefore pins the
/// same inputs minus the versions it is not allowed to go and look up.
fn build_closure(
    flow: &flow::Flow,
    flow_path: &Path,
    profiles: &[PathBuf],
    workspace: &flow::WorkspacePlan,
    tool_versions: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Result<closure::Closure, String> {
    let mut cl = closure::Closure::new();
    cl.set("sfh_version", json!(VERSION));
    cl.set_file("flow", flow_path);
    // The merged configuration is pinned by DIGEST, not embedded: the full
    // document is several kilobytes of JSON that would drown the closure file
    // a human is meant to read when a resume is refused, and the digest answers
    // the only question the closure asks - did it change.
    cl.set(
        "effective_config",
        json!({ "sha256": sha256::hex(flow.effective_config_json()?.as_bytes()) }),
    );
    for (i, p) in profiles.iter().enumerate() {
        cl.set_file(&format!("profile_overlay.{i}"), p);
    }
    // Context files are pinned by CONTENT: an edited TASK.md changes what the
    // run means, while leaving the flow byte-identical.
    let flow_dir = flow_path.parent().unwrap_or(Path::new("."));
    for (name, source) in &flow.contexts {
        match source.kind() {
            Ok("file") => {
                let raw = source.file.clone().unwrap_or_default();
                cl.set_file(
                    &format!("context.{name}"),
                    &context::resolve_source_path(flow_dir, &raw),
                );
            }
            // An inline or template context is already part of the flow's
            // bytes, but pinning it by name keeps the closure readable and
            // survives a future where a template can reach outside the file.
            Ok(kind) => {
                cl.set(
                    &format!("context.{name}"),
                    json!({"kind": kind, "sha256": sha256::hex(
                        source.inline.as_deref().or(source.template.as_deref()).unwrap_or("").as_bytes()
                    )}),
                );
            }
            Err(e) => return Err(format!("contexts.{name}: {e}")),
        }
    }
    cl.set(
        "workspace",
        json!({
            "mode": workspace.resolved.as_str(),
            "root": workspace.root,
            "base": workspace.base,
        }),
    );
    cl.set("unsafe_overrides", json!(flow.unsafe_overrides()));
    if let Some(tools) = tool_versions {
        cl.set("tools", serde_json::Value::Object(tools.clone()));
    }
    Ok(cl)
}

/// Where every `kind: file` context's frozen copy lives, relative to the run
/// dir, and the durable record of what run start managed to pin.
const CONTEXT_SNAPSHOT_DIR: &str = "context-snapshot";
const CONTEXT_SNAPSHOT_MANIFEST: &str = "context-snapshot.json";

/// What `snapshot_file_contexts` produced: the in-memory registry handed
/// straight to `context::activate_snapshot`, and the same per-source facts
/// written to BOTH the durable manifest and the run log (they answer the same
/// question - "what did this run actually read" - for two different
/// readers, so there is exactly one place that computes the answer).
#[derive(Debug)]
struct ContextSnapshot {
    registry: HashMap<String, Option<PathBuf>>,
    sources: Vec<serde_json::Value>,
    /// Whether the flow declared at least one `kind: file` context at all -
    /// the same fact that already decides, below, whether this function
    /// creates `CONTEXT_SNAPSHOT_DIR`. Deliberately NOT the same thing as
    /// `sources` being non-empty: a source that WAS declared but failed
    /// validation (a missing required file, a refused symlink) is deferred
    /// rather than recorded - see the `Err` arm below - so `sources` stays
    /// empty for that case too, and collapsing the two would make "this flow
    /// asked for nothing" indistinguishable from "this flow asked for
    /// something sfh could not get". `snapshot_and_persist_context` uses this
    /// field, not `sources`, to decide whether a manifest is worth writing at
    /// all.
    has_file_contexts: bool,
}

/// Freeze every `kind: file` context this flow declares, once, so the rest of
/// this run reads the SAME bytes `build_closure` just pinned instead of
/// opening the declared path again on every step.
///
/// P0-04: without this, an edited TASK.md between the `analyze` and
/// `implement` steps of one run silently changed what `implement` was
/// handed, while execution-closure.json went on recording the hash from
/// before the edit - the run's own record of what it read disagreed with
/// what it actually read, and no resume was involved.
///
/// Unconditional over `flow.contexts` - the same set `build_closure` iterates
/// above, not just the sources some step happens to name - so the run's
/// pinned copies and the closure's pinned hashes are always talking about the
/// same files. `inline` and `template` sources are left alone: an inline
/// source is already part of the flow's own bytes, and a template is
/// deliberately re-rendered per step (it may read `{{steps.x.output}}`,
/// which does not exist yet at run start, so freezing it here would be
/// wrong, not merely unnecessary).
///
/// A source that fails validation outright here - a symlink without
/// `allow_external`, a required file that is missing - is left OUT of the
/// registry rather than aborting the run. sfh has never refused a run merely
/// for DECLARING a broken context nobody uses; that lenience is preserved by
/// deferring to `context::build`'s own live-path fallback, which raises
/// exactly the same error the first time some step actually names it.
fn snapshot_file_contexts(
    flow: &flow::Flow,
    containment: &context::Containment,
    run_dir: &Path,
) -> Result<ContextSnapshot, String> {
    let file_contexts: Vec<(&String, &flow::ContextSource)> = flow
        .contexts
        .iter()
        .filter(|(_, s)| matches!(s.kind(), Ok("file")))
        .collect();
    let has_file_contexts = !file_contexts.is_empty();
    let mut registry: HashMap<String, Option<PathBuf>> = HashMap::new();
    let mut sources = Vec::new();
    if file_contexts.is_empty() {
        return Ok(ContextSnapshot {
            registry,
            sources,
            has_file_contexts,
        });
    }
    let snapshot_dir = run_dir.join(CONTEXT_SNAPSHOT_DIR);
    contain::mkdir_private(&snapshot_dir).map_err(|e| {
        format!(
            "cannot persist context snapshot: cannot create {}: {e}",
            snapshot_dir.display()
        )
    })?;
    for (name, source) in file_contexts {
        let raw = source.file.clone().unwrap_or_default();
        let external = source.allow_external.unwrap_or(false);
        let optional = source.optional.unwrap_or(false);
        match context::read_file_source(name, &raw, external, containment, optional) {
            Ok(Some(text)) => {
                // Named by a digest of the CONTEXT NAME, never the declared
                // path: a name is a flow-author identifier, but the identical
                // name reused across an unrelated OS's path separators or
                // reserved characters would turn straight into a path
                // injection if used as a file name verbatim (the same reason
                // render_bundle escapes a name before it can forge a
                // delimiter). The declared path is preserved as plain data in
                // `source`, below, for a human reading the manifest.
                let file_name = format!("{}.snapshot", sha256::hex(name.as_bytes()));
                let abs_path = snapshot_dir.join(&file_name);
                contain::write_private_atomic(&abs_path, &text).map_err(|e| {
                    format!("cannot persist context snapshot for contexts.{name}: {e}")
                })?;
                registry.insert(name.clone(), Some(abs_path));
                sources.push(json!({
                    "name": name, "source": raw, "state": "captured",
                    "sha256": sha256::hex(text.as_bytes()), "bytes": text.len() as u64,
                    "snapshot": format!("{CONTEXT_SNAPSHOT_DIR}/{file_name}"),
                }));
            }
            Ok(None) => {
                registry.insert(name.clone(), None);
                sources.push(json!({"name": name, "source": raw, "state": "absent"}));
            }
            // Deferred, not fatal - see the doc comment above.
            Err(_) => {}
        }
    }
    Ok(ContextSnapshot {
        registry,
        sources,
        has_file_contexts,
    })
}

/// Snapshot this flow's `kind: file` contexts at run start (see
/// `snapshot_file_contexts`, immediately above) and persist the result for a
/// later `--resume` to read back - UNLESS there is nothing to persist. A flow
/// that declares no `kind: file` context at all always produces an empty
/// registry and an empty `sources` list, and writing `CONTEXT_SNAPSHOT_MANIFEST`
/// for that is a manifest that documents nothing, paid for with an atomic
/// write and an fsync'd log line at the start of every single run - including
/// the overwhelming majority of flows that never touch this feature.
///
/// Skipping it is safe on resume specifically BECAUSE there was nothing to
/// pin: `ResumedSnapshot::NotPresent`'s own doc comment already treats "no
/// manifest at all" and "a flow that declared no `kind: file` context" as
/// the same, harmless case - both fall every step back to a live read. And a
/// live read is exactly what a step in THIS run would already do: with no
/// `kind: file` context declared, `context::build`'s snapshot lookup is never
/// even consulted for a "file" kind here, so there is no pinned fact for a
/// resume to disagree with in the first place. See
/// `context::snapshot_lookup`'s doc comment for the underlying design fact
/// this leans on - an empty pinned map and no pin at all are indistinguishable
/// to any lookup, by construction.
///
/// Returns the registry to pin for the rest of THIS run either way - an empty
/// one has the identical effect on `context::activate_snapshot` whether or
/// not it is ever written down, so the guard the caller installs is
/// unaffected by which branch below ran.
fn snapshot_and_persist_context(
    flow: &flow::Flow,
    containment: &context::Containment,
    run_dir: &Path,
    log: &mut std::fs::File,
) -> Result<HashMap<String, Option<PathBuf>>, String> {
    let snap = snapshot_file_contexts(flow, containment, run_dir)?;
    if snap.has_file_contexts {
        let manifest_text = serde_json::to_string_pretty(&json!({
            "schema_version": 1,
            "sources": snap.sources,
        }))
        .map_err(|e| format!("cannot serialize the context snapshot: {e}"))?;
        contain::write_private_atomic(&run_dir.join(CONTEXT_SNAPSHOT_MANIFEST), manifest_text)
            .map_err(|e| format!("cannot persist context snapshot: {e}"))?;
        // Recorded the same way the closure fingerprint is, right below the
        // event that pins it, so "what this run actually read" is answerable
        // from log.jsonl alone without also locating the manifest file.
        log_event(
            log,
            json!({"ts": utc_stamp(), "event": "context_snapshot", "sources": snap.sources}),
        )?;
    }
    Ok(snap.registry)
}

/// What a resumed run finds for the context snapshot the ORIGINAL attempt
/// captured.
#[derive(Debug)]
enum ResumedSnapshot {
    /// No manifest at all: a run dir from before this feature existed, or one
    /// whose flow declared no `kind: file` context. Steps fall back to the
    /// live path, exactly as every run did before this fix - there is
    /// nothing to disagree with, because nothing was ever pinned.
    NotPresent,
    /// Read back and validated; every step now sees exactly what run start
    /// pinned, the same as a fresh run would.
    Loaded(HashMap<String, Option<PathBuf>>),
    /// The manifest is there but sfh cannot make sense of its CONTENT - which
    /// should be impossible for a file only sfh ever writes, atomically, into
    /// a private run dir. Treated as an execution-closure-level problem (the
    /// caller offers the same refusal and the same --force-resume escape
    /// hatch the closure mismatch above does) rather than silently falling
    /// back to live reads that might not agree with what
    /// execution-closure.json pinned.
    Corrupt(String),
}

/// Read back what `snapshot_file_contexts` recorded, so a resume pins its
/// steps to the SAME bytes the original attempt did rather than capturing a
/// fresh copy of whatever is on disk now - see the resume-semantics comment
/// at this function's call site for why re-capturing on resume would be
/// wrong even under --force-resume.
fn load_context_snapshot(run_dir: &Path) -> Result<ResumedSnapshot, String> {
    // Contained, no-follow read: a run dir is untrusted input on --resume
    // (rev_break #6 already treats meta.json and log.jsonl this way), so a
    // symlink planted at this fixed name is refused unconditionally rather
    // than folded into the waivable "Corrupt" case below.
    let Some(text) = contain::read_contained_opt(run_dir, CONTEXT_SNAPSHOT_MANIFEST)? else {
        return Ok(ResumedSnapshot::NotPresent);
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            return Ok(ResumedSnapshot::Corrupt(format!(
                "{CONTEXT_SNAPSHOT_MANIFEST} is not valid JSON: {e}"
            )))
        }
    };
    let Some(entries) = v.get("sources").and_then(|s| s.as_array()) else {
        return Ok(ResumedSnapshot::Corrupt(format!(
            "{CONTEXT_SNAPSHOT_MANIFEST} has no 'sources' array"
        )));
    };
    let mut map = HashMap::new();
    for entry in entries {
        let Some(name) = entry.get("name").and_then(|x| x.as_str()) else {
            return Ok(ResumedSnapshot::Corrupt(format!(
                "{CONTEXT_SNAPSHOT_MANIFEST}: an entry has no 'name'"
            )));
        };
        match entry.get("state").and_then(|x| x.as_str()) {
            Some("absent") => {
                map.insert(name.to_string(), None);
            }
            Some("captured") => {
                let Some(rel) = entry.get("snapshot").and_then(|x| x.as_str()) else {
                    return Ok(ResumedSnapshot::Corrupt(format!(
                        "{CONTEXT_SNAPSHOT_MANIFEST}: contexts.{name} is 'captured' but names no snapshot file"
                    )));
                };
                // A path that resolves outside run_dir means the run dir was
                // tampered with, not merely that the record is stale - the
                // same class of attack meta.json is already read contained
                // against - so it is propagated unconditionally (rev_break
                // #6) instead of folded into the waivable case below.
                match contain::contained_opt(run_dir, rel)? {
                    Some(path) => {
                        map.insert(name.to_string(), Some(path));
                    }
                    None => {
                        return Ok(ResumedSnapshot::Corrupt(format!(
                            "{CONTEXT_SNAPSHOT_MANIFEST}: contexts.{name}'s pinned snapshot {rel} is missing"
                        )))
                    }
                }
            }
            other => {
                return Ok(ResumedSnapshot::Corrupt(format!(
                "{CONTEXT_SNAPSHOT_MANIFEST}: contexts.{name} has an unrecognised state {other:?}"
            )))
            }
        }
    }
    Ok(ResumedSnapshot::Loaded(map))
}

fn log_event(f: &mut std::fs::File, mut v: serde_json::Value) -> Result<(), String> {
    if let Some(object) = v.as_object_mut() {
        object
            .entry("schema_version".to_string())
            .or_insert_with(|| json!(1));
    }
    writeln!(f, "{v}").map_err(|e| format!("cannot append durable run log: {e}"))?;
    f.flush()
        .map_err(|e| format!("cannot flush durable run log: {e}"))?;
    f.sync_data()
        .map_err(|e| format!("cannot sync durable run log: {e}"))
}

/// The fan-out members' own step_start lines: one per member about to run, each
/// naming the session it attached to.
///
/// `parallel:` with a `fork_from:` on every child is the documented shape of
/// session reuse, so recording `session_parent` for top-level leaves only left
/// the exact case the key was added for unrecorded - and after the flow edit
/// that makes the log the only account of what ran, unrecoverable.
///
/// The `parent` key is what tells these apart from a top-level step_start:
/// load_resume uses step_start to remember "this started and never ended, so
/// resume here", and a member id is not a place a resume can start. The group's
/// own group_start / foreach_start already stands for the whole fan-out there.
fn log_member_starts<'a, I>(
    f: &mut std::fs::File,
    group: &str,
    visit: u32,
    members: I,
) -> Result<(), String>
where
    I: Iterator<Item = (String, &'a leaf::Prepared)>,
{
    for (id, prep) in members {
        log_event(
            f,
            json!({"ts": utc_stamp(), "event": "step_start", "step": id, "parent": group,
                   "visit": visit, "cmd": prep.inv.describe(),
                   "session_parent": session_parent_json(prep),
                   "protocol_expected": protocol::expected_kind(&prep.parse),
                   "context_hash": prep.context_hash,
                   "context_file": prep.context_file.as_ref().and_then(|p| file_name(p))}),
        )?;
    }
    Ok(())
}

/// The session a step attached to, for step_start. Null - like `session` in
/// step_end - when the step opened its own context.
fn session_parent_json(prep: &leaf::Prepared) -> serde_json::Value {
    match &prep.session_parent {
        Some(p) => json!({"mode": p.mode, "step": p.step, "tool": p.tool, "id": p.id}),
        None => serde_json::Value::Null,
    }
}

/// Remember, in THIS run's log, the fan-out members a resume carried over
/// instead of executing. Without it the carry-over lives only in memory: the
/// skipped members write no step_end, so a second crash in the same fan-out
/// leaves the next resume with no record of them and they run - and bill - a
/// second time. Sorted so the log is reproducible.
///
/// ONE event carrying the whole set, not one line per member. Written a line
/// at a time, a kill part-way through left a PARTIAL set recorded against the
/// new visit, and the carry-forward would not replace it - the members whose
/// line had not been written yet ran again. A single line is either read whole
/// or, torn, fails to parse and is skipped like any other torn line, which
/// leaves the previous visit's carry standing and nothing lost.
fn log_restored_members(
    f: &mut std::fs::File,
    group: &str,
    visit: u32,
    restored: &HashSet<String>,
) -> Result<(), String> {
    if restored.is_empty() {
        return Ok(());
    }
    let mut names: Vec<&String> = restored.iter().collect();
    names.sort();
    log_event(
        f,
        json!({"ts": utc_stamp(), "event": "members_restored", "steps": names, "parent": group, "visit": visit}),
    )
}

#[derive(Clone, Copy)]
enum PositionVia {
    Rule,
    CatchAll,
    Fallthrough,
    OnError,
    MaxVisits,
    /// The `on_budget` landing. The only via whose `after` names a step that
    /// did NOT run: the jump happens on entry to it, not after it.
    Budget,
}

impl PositionVia {
    fn as_str(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::CatchAll => "catch_all",
            Self::Fallthrough => "fallthrough",
            Self::OnError => "on_error",
            Self::MaxVisits => "max_visits",
            Self::Budget => "budget",
        }
    }
}

/// How many characters of routing text a position event records. The same cut
/// the fan-out member snapshot uses: enough to recognise a verdict line, not
/// enough for a runaway step to bloat log.jsonl.
pub(crate) const ROUTE_LINE_CHARS: usize = 200;

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The text a rule was judged on, for the log. Whole-text predicates are not
/// judged on a line at all, so they record the head of the routing text;
/// everything else - the last-line predicates and the catch-all - records the
/// last non-empty line, which is what they compared against.
fn route_line_of(r: &flow::Route, route_text: &str, last: &str) -> String {
    if r.when_contains.is_some() || r.when_matches.is_some() {
        clip(route_text, ROUTE_LINE_CHARS)
    } else {
        clip(last, ROUTE_LINE_CHARS)
    }
}

/// One fan-out member's contribution to a `when_members` tally.
///
/// Deliberately not derived from the group's routing text. That text is the
/// members' raw output concatenated, and a FAILED member's output goes into it
/// unmarked - the "[sfh: FAILED]" banner is added to the labeled aggregate
/// only. In the text, "said the winning line" and "said the winning line and
/// exited 1" are the same bytes; here they are different facts.
#[derive(Clone)]
struct MemberVerdict {
    id: String,
    /// `LeafDone::ok()` for a member this process ran; the recorded equivalent
    /// for one restored from a crashed attempt.
    ok: bool,
    exit: i32,
    /// The trimmed last non-empty line of the member's own pre-compact output,
    /// cut to ROUTE_LINE_CHARS. Cut on the way IN rather than on the way to the
    /// log, because the tally compares this value: cutting only for the log
    /// would let an over-long verdict match live and miss after a resume.
    last_line: String,
    /// Whether that cut removed anything. A cut line is a PREFIX, and comparing
    /// a prefix for equality is how a member that said the verdict and then kept
    /// talking gets counted as having voted - the needle may itself be
    /// ROUTE_LINE_CHARS long, so "equal after cutting" does not imply "equal".
    /// The full line is not kept (that is what the cut is for), so the honest
    /// answer to "did this member say exactly that" is "cannot tell", and
    /// invariant 6 puts that on the not-matching side.
    clipped: bool,
}

impl MemberVerdict {
    fn new(id: &str, ok: bool, exit: i32, output: &str) -> Self {
        let full = leaf::last_line(output);
        Self {
            id: id.to_string(),
            ok,
            exit,
            last_line: clip(full, ROUTE_LINE_CHARS),
            clipped: full.chars().count() > ROUTE_LINE_CHARS,
        }
    }

    fn to_json(&self) -> serde_json::Value {
        json!({"id": self.id, "ok": self.ok, "exit": self.exit,
               "last_line": self.last_line, "clipped": self.clipped})
    }
}

/// What the router knows about the step's fan-out members.
enum Members<'a> {
    /// Not a fan-out at all, so there is nobody to count. validate refuses
    /// `when_members` on such a step; if a rule reaches the router anyway it
    /// decides nothing.
    NotAGroup,
    Known(&'a [MemberVerdict]),
    /// A fan-out whose recorded aggregate_end carries no per-member records.
    Unrecorded,
    /// A `parallel:` group whose recorded member count is not the one the flow
    /// now declares - only reachable by editing the group and `--force-resume`.
    Mismatch {
        recorded: usize,
        declared: usize,
    },
}

impl Members<'_> {
    /// Who voted, and whether that is enough. `Ok(None)` = the rule does not
    /// match; `Err` = the question cannot be answered honestly at all.
    fn tally(
        &self,
        wm: &flow::WhenMembers,
        needle: &str,
        step: &str,
        idx: usize,
    ) -> Result<Option<(u32, Vec<String>)>, String> {
        let ms = match self {
            Self::NotAGroup => return Ok(None),
            // Only reachable by editing when_members into a flow and resuming a
            // run recorded before per-member records existed. Falling through
            // to the catch-all would make the branch depend on which sfh wrote
            // the run, silently, so the run stops and says so.
            Self::Unrecorded => {
                return Err(format!(
                    "step '{step}' route[{idx}]: this run predates per-member route records; re-run the group step or remove when_members"
                ))
            }
            // Counting the old votes under the new quantifier would answer a
            // question about a group that no longer exists, and `all: true`
            // would answer it YES - fail-open on the one predicate whose whole
            // job is to be a gate. Stop and say which two numbers disagree.
            Self::Mismatch { recorded, declared } => {
                return Err(format!(
                    "step '{step}' route[{idx}]: the recorded vote has {recorded} member(s) but the flow now declares {declared}; re-run the group step so every declared member votes, or resume with the flow the record was made under"
                ))
            }
            Self::Known(m) => *m,
        };
        let voters: Vec<String> = ms
            .iter()
            .filter(|m| m.ok && !m.clipped && m.last_line == needle)
            .map(|m| m.id.clone())
            .collect();
        let votes = voters.len();
        // A fan-out that produced no members has decided nothing. `all: true`
        // over the empty set is true in logic and fail-OPEN here, which is the
        // one thing a gate may never be (invariant 6).
        let enough = !ms.is_empty()
            && match (wm.at_least, wm.all) {
                (Some(n), _) => votes as u64 >= u64::from(n),
                (None, Some(true)) => votes == ms.len(),
                _ => false,
            };
        Ok(enough.then_some((votes as u32, voters)))
    }
}

/// A route rule that fired, and why - written into the position event so the
/// routing decision can be read back without re-running the flow.
struct RouteHit {
    goto: String,
    via: PositionVia,
    /// 0-based index into the step's `route:` list.
    rule: usize,
    /// See `route_line_of`.
    line: String,
    /// Filled in by member-quantified rules (F1); `log_position` omits both
    /// keys while they are None, so no other caller has to know about them.
    votes: Option<u32>,
    voters: Option<Vec<String>>,
    protocol: Option<protocol::ProtocolState>,
}

/// How much of a step's stderr `when_stderr_matches` will read. A step is free
/// to produce gigabytes of it; routing is not the place to hold them.
const STDERR_MATCH_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// The stderr text `when_stderr_matches` judges, or None when there is none to
/// judge. BOTH live and resumed runs come through here - the path is the one
/// `{{steps.<id>.stderr_file}}` exposes, which live sets from the file just
/// written and a resume restores (already containment-checked) from step_end -
/// so the two paths read the same bytes rather than one reading memory and the
/// other a file.
///
/// None means "no evidence": the step never recorded a stderr file (a fan-out
/// group has none), or the file is gone from the run dir. A path that resolves
/// OUTSIDE the run dir is not absence, it is tampering, and stays an error.
fn stderr_text_for(
    step_id: &str,
    ctx: &template::Ctx<'_>,
    run_dir: &Path,
) -> Result<Option<String>, String> {
    let Some(o) = ctx.outputs.get(step_id) else {
        return Ok(None);
    };
    if o.stderr_file.is_empty() {
        return Ok(None);
    }
    contain::read_contained_abs_capped(run_dir, Path::new(&o.stderr_file), STDERR_MATCH_MAX_BYTES)
}

fn evaluate_route(
    step: &flow::Step,
    route_text: &str,
    ctx: &template::Ctx<'_>,
    run_dir: &Path,
    members: Members<'_>,
    protocol_state: Option<protocol::ProtocolState>,
) -> Result<Option<RouteHit>, String> {
    let last = leaf::last_line(route_text).to_string();
    // Read at most once per routing decision, and only when a rule asks.
    let mut stderr_seen: Option<Option<String>> = None;
    for (idx, r) in step.route.iter().enumerate() {
        let mut matched = true;
        let mut check = |needle: &Option<String>, hay: &str, is_rx: bool| -> Result<(), String> {
            if !matched {
                return Ok(());
            }
            let Some(t) = needle else { return Ok(()) };
            let t = template::render(t, ctx)?;
            let hit = if is_rx {
                regex::Regex::new(&t)
                    .map_err(|e| format!("step '{}' route regex: {e}", step.id))?
                    .is_match(hay)
            } else {
                hay.contains(&t)
            };
            if !hit {
                matched = false;
            }
            Ok(())
        };
        check(&r.when_contains, route_text, false)?;
        check(&r.when_matches, route_text, true)?;
        check(&r.when_last_line_contains, &last, false)?;
        check(&r.when_last_line_matches, &last, true)?;
        if matched {
            if let Some(t) = &r.when_last_line_is {
                if last != template::render(t, ctx)? {
                    matched = false;
                }
            }
        }
        // Exclusive with everything above (validate), so this decides the rule
        // on its own; the guard only keeps a hand-built Route from rendering a
        // template it does not need.
        let mut tally = None;
        if matched {
            if let Some(wm) = &r.when_members {
                let needle = template::render(&wm.last_line_is, ctx)?;
                match members.tally(wm, &needle, &step.id, idx)? {
                    Some(t) => tally = Some(t),
                    None => matched = false,
                }
            }
        }
        if matched {
            if let Some(want) = r.when_exit {
                // The step's own normalized exit, read back out of `outputs`:
                // live inserts it just before routing, and a resume restores it
                // from step_end, so there is nothing extra to persist. A step
                // with no recorded output has no exit to compare - fail closed.
                if ctx.outputs.get(&step.id).map(|o| o.exit) != Some(want) {
                    matched = false;
                }
            }
        }
        if matched {
            if let Some(want) = &r.when_label_is {
                // Rendered like the other text conditions, then compared for
                // equality. A step with no declared outcome has no label, and
                // an empty label never matches - fail closed, the same way
                // when_exit does without a recorded output.
                let want = template::render(want, ctx)?;
                let got = ctx.outputs.get(&step.id).map(|o| o.label.as_str());
                matched = !want.is_empty() && got == Some(want.as_str());
            }
        }
        if matched {
            if let Some(want) = r.when_outcome_is {
                matched =
                    ctx.outputs.get(&step.id).map(|o| o.outcome.as_str()) == Some(want.as_str());
            }
        }
        if matched {
            if let Some(want) = r.when_protocol_is {
                matched = protocol_state == Some(want);
            }
        }
        if matched {
            if let Some(t) = &r.when_stderr_matches {
                let t = template::render(t, ctx)?;
                let rx = regex::Regex::new(&t)
                    .map_err(|e| format!("step '{}' route regex: {e}", step.id))?;
                if stderr_seen.is_none() {
                    stderr_seen = Some(stderr_text_for(&step.id, ctx, run_dir)?);
                }
                // The outer Option is the read cache, the inner one is whether
                // there was any stderr to judge. No stderr = no match.
                matched = match stderr_seen.as_ref().and_then(|t| t.as_deref()) {
                    Some(s) => rx.is_match(s),
                    None => false,
                };
            }
        }
        if matched {
            let via = if r.is_catch_all() {
                PositionVia::CatchAll
            } else {
                PositionVia::Rule
            };
            return Ok(Some(RouteHit {
                goto: r.goto.clone(),
                via,
                rule: idx,
                line: route_line_of(r, route_text, &last),
                votes: tally.as_ref().map(|(n, _)| *n),
                voters: tally.map(|(_, v)| v),
                protocol: r.when_protocol_is.and(protocol_state),
            }));
        }
    }
    Ok(None)
}

/// `hit` is Some only for the two vias that come from evaluating `route:`
/// (rule / catch_all); on_error, max_visits and fallthrough judged no rule, so
/// they carry no rule index and no routing text.
fn log_position(
    f: &mut std::fs::File,
    after: &str,
    next: String,
    via: PositionVia,
    hit: Option<&RouteHit>,
) -> Result<(), String> {
    let mut ev = json!({
        "ts": utc_stamp(), "event": "position", "after": after,
        "next": next, "via": via.as_str(),
    });
    if let (Some(h), Some(o)) = (hit, ev.as_object_mut()) {
        o.insert("rule".into(), json!(h.rule));
        o.insert("route_line".into(), json!(h.line));
        if let Some(n) = h.votes {
            o.insert("votes".into(), json!(n));
        }
        if let Some(v) = &h.voters {
            o.insert("voters".into(), json!(v));
        }
        if let Some(state) = h.protocol {
            o.insert("protocol_state".into(), json!(state.as_str()));
        }
    }
    log_event(f, ev)
}

fn apply_on_error(
    log: &mut std::fs::File,
    step: &flow::Step,
    index_of: &HashMap<String, usize>,
    run_dir: &Path,
    failure_code: Option<machine::ErrorCode>,
) -> Result<ErrorDisposition, String> {
    match step.on_error.as_deref().unwrap_or("fail") {
        "continue" => Ok(ErrorDisposition::Continue),
        oe if oe.starts_with("goto:") => match &oe[5..] {
            "end" => {
                log_position(log, &step.id, "end".into(), PositionVia::OnError, None)?;
                Ok(ErrorDisposition::Completed)
            }
            "fail" => {
                log_position(log, &step.id, "fail".into(), PositionVia::OnError, None)?;
                Err(format!(
                    "step '{}' failed and on_error routed to fail",
                    step.id
                ))
            }
            "stuck" => {
                log_position(log, &step.id, "stuck".into(), PositionVia::OnError, None)?;
                Ok(ErrorDisposition::Stuck)
            }
            id => {
                let next = index_of.get(id).copied().ok_or_else(|| {
                    format!("step '{}': on_error goto target '{id}' not found", step.id)
                })?;
                log_position(log, &step.id, id.to_string(), PositionVia::OnError, None)?;
                Ok(ErrorDisposition::Goto(next))
            }
        },
        _ => Err(match failure_code {
            Some(code) => format!(
                "{}: step '{}' failed - see {}",
                code.as_str(),
                step.id,
                run_dir.display()
            ),
            None => format!("step '{}' failed - see {}", step.id, run_dir.display()),
        }),
    }
}

fn protocol_failure_code(state: Option<protocol::ProtocolState>) -> Option<machine::ErrorCode> {
    match state {
        Some(protocol::ProtocolState::MissingTerminal) => Some(machine::ErrorCode::TerminalMissing),
        Some(protocol::ProtocolState::Invalid) => Some(machine::ErrorCode::ProtocolInvalid),
        _ => None,
    }
}

/// Persist one completed attempt and, when present, the fallback profile whose
/// execution is now required. Keeping both facts in one synced JSONL record
/// closes the crash window between "attempt failed" and "fallback started".
fn log_persistence_failure(
    f: &mut std::fs::File,
    step: &str,
    parent: Option<&str>,
    visit: u32,
    d: &leaf::LeafDone,
    error: &str,
) -> Result<(), String> {
    log_event(
        f,
        json!({
            "ts": utc_stamp(),
            "event": "persistence_failure",
            "step": step,
            "parent": parent,
            "visit": visit,
            "error": error,
            "attempts": d.attempts,
            "input_tokens": d.usage.input_tokens,
            "output_tokens": d.usage.output_tokens,
            "cost_usd": d.usage.cost_usd,
            "tool": d.tool,
        }),
    )
}

fn log_step_end_with_next(
    f: &mut std::fs::File,
    step: &str,
    parent: Option<&str>,
    visit: u32,
    d: &leaf::LeafDone,
    next_fallback: Option<&str>,
    postprocess_pending: bool,
) -> Result<(), String> {
    let session = match (&d.tool, &d.session_id) {
        (Some(t), Some(id)) => {
            json!({"tool": t, "id": id, "cwd": d.cwd, "marker": d.session_marker, "access": d.access.map(|a| a.as_str())})
        }
        _ => serde_json::Value::Null,
    };
    log_event(
        f,
        json!({
            "ts": utc_stamp(), "event": "step_end", "step": step, "parent": parent,
            "next_fallback": next_fallback,
            "postprocess_pending": postprocess_pending,
            "visit": visit, "exit": d.exit_code, "timed_out": d.timed_out,
            // A signal handler may reap the process group before run_cmd
            // observes the flag. Snapshot the run-level cancellation at the
            // durable commit point too, so resume never mistakes that race for
            // an ordinary completed leaf.
            "interrupted": d.interrupted || execute::interrupted(),
            "attempts": d.attempts, "dur_ms": d.dur_ms as u64,
            "retry_budget_exhausted": d.retry_budget_exhausted,
            "idle_ms": d.idle_ms,
            "output_chars": d.chain_output.chars().count(),
            "output_hash": fingerprint(&d.chain_output),
            "input_tokens": d.usage.input_tokens, "output_tokens": d.usage.output_tokens,
            "cost_usd": d.usage.cost_usd, "tool": d.tool,
            "chain_file": file_name(&d.out_file).map(|n| n.replace(".out.txt", ".chain.txt")),
            "out_file": file_name(&d.out_file),
            "cmd": d.cmd, "session": session,
            "harness_diagnostic": d.harness_diagnostic.as_deref().map(|s| one_line(s, 500)),
            // Additive protocol evidence (spec 15.1). A reader that predates
            // these keys sees the same event it always did; a reader that wants
            // them can tell "the tool failed" from "sfh could not verify that
            // the tool finished" without re-parsing the raw artifact.
            "protocol_state": d.protocol.protocol.as_str(),
            "terminal_seen": d.protocol.terminal_seen,
            "terminal_success": d.protocol.terminal_success,
            "final_message_seen": d.protocol.final_message_seen,
            "malformed_records": d.protocol.malformed_records,
            // What this step's exit code was DECLARED to mean, when the flow
            // said. Persisted so a resumed run routes on the same answer the
            // live one did instead of re-deriving it from an exit code sfh may
            // already have normalized. Null for the ordinary step, and for
            // every run written before these keys existed.
            "outcome": d.outcome.as_ref().map(|(r, _)| r.as_str()),
            "outcome_label": d.outcome.as_ref().and_then(|(_, l)| l.clone()),
            // Which OS produced this step. A log is routinely read on a
            // different machine from the one that wrote it, and "it passes on
            // mine" is exactly the class of report this answers.
            "os": std::env::consts::OS,
        }),
    )
}

/// How a fan-out lap ended, as the log records it. A struct rather than eight
/// positional arguments, so the parallel and foreach branches cannot drift into
/// writing different shapes of the same event.
struct AggregateEnd<'a> {
    step: &'a str,
    visit: u32,
    gtag: &'a str,
    failed: bool,
    plain: &'a str,
    plain_file: &'a str,
    members: &'a [MemberVerdict],
    postprocess_pending: bool,
}

fn log_aggregate_end(f: &mut std::fs::File, a: AggregateEnd<'_>) -> Result<(), String> {
    log_event(
        f,
        json!({
            "ts": utc_stamp(), "event": "aggregate_end", "step": a.step, "visit": a.visit,
            "failed": a.failed, "exit": if a.failed { 1 } else { 0 },
            "output_hash": fingerprint(a.plain),
            "chain_file": format!("{}.chain.txt", a.gtag), "out_file": format!("{}.out.txt", a.gtag),
            "plain_file": a.plain_file,
            "postprocess_pending": a.postprocess_pending,
            // Who said what, per member. A resume re-decides a `when_members`
            // route from THIS and nothing else: the artifacts on disk hold the
            // members' text but not which of them completed, and the text alone
            // cannot answer that (see MemberVerdict).
            "members": a.members.iter().map(MemberVerdict::to_json).collect::<Vec<_>>(),
        }),
    )
}

fn file_name(p: &Path) -> Option<String> {
    p.file_name().map(|n| n.to_string_lossy().into_owned())
}

fn stderr_file_for(out_file: &Path) -> PathBuf {
    let name = out_file
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".out.txt"))
        .map(|n| format!("{n}.err.txt"))
        .unwrap_or_else(|| "stderr.err.txt".to_string());
    out_file.with_file_name(name)
}

fn abs(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|c| c.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

pub fn utc_stamp() -> String {
    utc_stamp_at(execute::epoch_secs())
}

/// The same stamp for a recorded instant rather than for now.
pub fn utc_stamp_at(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Inverse of `utc_stamp`: "YYYYMMDD-HHMMSS" -> unix epoch seconds. `sfh status`
/// reports "how long ago" from stamps another process wrote, so it has to be
/// able to read its own format back. Anything that does not parse exactly is
/// None - a status.json is untrusted input, and a half-understood timestamp
/// must not become a confident number on screen.
pub fn parse_utc_stamp(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 15
        || b[8] != b'-'
        || !b
            .iter()
            .enumerate()
            .all(|(i, c)| i == 8 || c.is_ascii_digit())
    {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s[r].parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(4..6)?, n(6..8)?);
    let (h, mi, sec) = (n(9..11)?, n(11..13)?, n(13..15)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let days = days_from_civil(y, mo as u32, d as u32);
    u64::try_from(days * 86400 + h * 3600 + mi * 60 + sec).ok()
}

/// Howard Hinnant's days_from_civil: (y, m, d) UTC -> days since 1970-01-01.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Howard Hinnant's civil_from_days: days since 1970-01-01 -> (y, m, d), UTC.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- P0-04: the execution closure pins context files, but steps must
    // read the SAME bytes, not the live file ----

    fn context_flow(contexts_yaml: &str) -> flow::Flow {
        serde_yaml_ng::from_str(&format!(
            "name: t\ncontexts:\n{contexts_yaml}steps:\n  - id: a\n    cmd: [\"echo\", \"x\"]\n"
        ))
        .expect("test flow parses")
    }

    #[test]
    fn snapshot_file_contexts_freezes_file_sources_and_leaves_inline_and_template_untouched() {
        let dir = std::env::temp_dir().join(format!("sfh-snap-{}", contain::random_nonce()));
        let flow_dir = dir.join("flow");
        let run_dir = dir.join("run");
        contain::mkdir_private(&flow_dir).unwrap();
        contain::mkdir_private(&run_dir).unwrap();
        std::fs::write(flow_dir.join("TASK.md"), "do the thing").unwrap();
        let flow = context_flow(
            "  task:\n    file: \"TASK.md\"\n  notes:\n    file: \"notes.md\"\n    optional: true\n  rules:\n    inline: \"be nice\"\n  live:\n    template: \"{{vars.x}}\"\n",
        );
        let containment = context::Containment {
            flow_dir: &flow_dir,
            workspace: None,
        };

        let snap = snapshot_file_contexts(&flow, &containment, &run_dir).unwrap();

        // Only the two `kind: file` sources are captured at all.
        assert_eq!(snap.registry.len(), 2, "{:?}", snap.registry.keys());
        assert!(
            !snap.registry.contains_key("rules"),
            "inline is flow bytes already, never snapshotted"
        );
        assert!(
            !snap.registry.contains_key("live"),
            "a template must stay dynamic, never snapshotted"
        );

        let task_path = snap
            .registry
            .get("task")
            .cloned()
            .flatten()
            .expect("task captured");
        assert_eq!(std::fs::read_to_string(&task_path).unwrap(), "do the thing");
        assert!(
            task_path.starts_with(run_dir.join(CONTEXT_SNAPSHOT_DIR)),
            "{}",
            task_path.display()
        );
        assert_eq!(
            snap.registry.get("notes"),
            Some(&None),
            "optional and missing is pinned as absent"
        );

        assert_eq!(snap.sources.len(), 2);
        let by_name = |n: &str| snap.sources.iter().find(|s| s["name"] == n).unwrap();
        assert_eq!(by_name("task")["state"], "captured");
        assert_eq!(by_name("task")["source"], "TASK.md");
        assert_eq!(
            by_name("task")["sha256"],
            sha256::hex("do the thing".as_bytes())
        );
        assert_eq!(by_name("notes")["state"], "absent");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_file_contexts_defers_rather_than_aborts_a_source_that_fails_containment() {
        // A symlink without allow_external fails read_file_source's check.
        // snapshot_file_contexts must not let one bad declaration abort a run
        // whose steps might never even name it - it just leaves that name
        // unpinned, so context::build's live fallback raises the same error
        // it always has, the first time something actually asks for it.
        let dir =
            std::env::temp_dir().join(format!("sfh-snap-symlink-{}", contain::random_nonce()));
        let outside = dir.join("outside");
        contain::mkdir_private(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "TOP SECRET").unwrap();
        let flow_dir = dir.join("flow");
        contain::mkdir_private(&flow_dir).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), flow_dir.join("link.txt")).unwrap();
        let run_dir = dir.join("run");
        contain::mkdir_private(&run_dir).unwrap();
        let flow = context_flow("  esc:\n    file: \"link.txt\"\n");
        let containment = context::Containment {
            flow_dir: &flow_dir,
            workspace: None,
        };

        let snap = snapshot_file_contexts(&flow, &containment, &run_dir).unwrap();
        assert!(
            !snap.registry.contains_key("esc"),
            "a source that fails validation must not be pinned at all: {:?}",
            snap.registry.get("esc")
        );
        assert!(snap.sources.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_context_snapshot_that_cannot_be_persisted_fails_the_run_instead_of_falling_back_to_live_reads(
    ) {
        // The whole point of this feature is that a step never reads the
        // live path once a run has started; a snapshot sfh could not write
        // must not be the one case that quietly reopens that hole. It has to
        // fail run start the same way an unwritable execution-closure.json
        // or meta.json already does.
        let dir =
            std::env::temp_dir().join(format!("sfh-snap-nowrite-{}", contain::random_nonce()));
        let flow_dir = dir.join("flow");
        let run_dir = dir.join("run");
        contain::mkdir_private(&flow_dir).unwrap();
        contain::mkdir_private(&run_dir).unwrap();
        std::fs::write(flow_dir.join("TASK.md"), "content").unwrap();
        // A plain FILE sits where snapshot_file_contexts needs to create the
        // context-snapshot/ DIRECTORY, so mkdir_private fails.
        std::fs::write(run_dir.join(CONTEXT_SNAPSHOT_DIR), "not a directory").unwrap();
        let flow = context_flow("  task:\n    file: \"TASK.md\"\n");
        let containment = context::Containment {
            flow_dir: &flow_dir,
            workspace: None,
        };

        let err = snapshot_file_contexts(&flow, &containment, &run_dir).unwrap_err();
        assert!(err.contains("persist"), "{err}");
        assert_eq!(
            run_failure_code(&err),
            machine::ErrorCode::PersistenceFailure,
            "{err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_context_snapshot_round_trips_what_snapshot_file_contexts_wrote() {
        let dir = std::env::temp_dir().join(format!("sfh-snap-rt-{}", contain::random_nonce()));
        let flow_dir = dir.join("flow");
        let run_dir = dir.join("run");
        contain::mkdir_private(&flow_dir).unwrap();
        contain::mkdir_private(&run_dir).unwrap();
        std::fs::write(flow_dir.join("TASK.md"), "the original text").unwrap();
        let flow = context_flow("  task:\n    file: \"TASK.md\"\n  notes:\n    file: \"notes.md\"\n    optional: true\n");
        let containment = context::Containment {
            flow_dir: &flow_dir,
            workspace: None,
        };
        let snap = snapshot_file_contexts(&flow, &containment, &run_dir).unwrap();
        let manifest_text = serde_json::to_string_pretty(&json!({
            "schema_version": 1,
            "sources": snap.sources,
        }))
        .unwrap();
        contain::write_private_atomic(&run_dir.join(CONTEXT_SNAPSHOT_MANIFEST), manifest_text)
            .unwrap();

        let loaded = match load_context_snapshot(&run_dir).unwrap() {
            ResumedSnapshot::Loaded(map) => map,
            _ => panic!("expected the round-tripped manifest to load cleanly"),
        };
        assert_eq!(loaded.get("notes"), Some(&None));
        let task_path = loaded.get("task").cloned().flatten().expect("task loaded");
        assert_eq!(
            std::fs::read_to_string(&task_path).unwrap(),
            "the original text"
        );

        // No manifest at all: an older run dir, not a fault.
        let bare_run_dir = dir.join("bare-run");
        contain::mkdir_private(&bare_run_dir).unwrap();
        assert!(matches!(
            load_context_snapshot(&bare_run_dir).unwrap(),
            ResumedSnapshot::NotPresent
        ));

        // A manifest that exists but is not valid JSON must be reported, not
        // silently treated as "nothing pinned".
        let broken_run_dir = dir.join("broken-run");
        contain::mkdir_private(&broken_run_dir).unwrap();
        contain::write_private_atomic(&broken_run_dir.join(CONTEXT_SNAPSHOT_MANIFEST), "not json")
            .unwrap();
        assert!(matches!(
            load_context_snapshot(&broken_run_dir).unwrap(),
            ResumedSnapshot::Corrupt(_)
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A flow that declares no `contexts:` at all used to still get
    /// `context-snapshot.json` written into its run dir - a manifest naming
    /// zero sources, on every single run, paid for with an atomic write and
    /// an fsync'd log line for a feature the flow never touches. This is the
    /// exact scenario a live `sfh run` against such a flow reproduces; see
    /// `snapshot_and_persist_context`'s doc comment for why skipping both
    /// artifacts here is safe on a later `--resume`.
    #[test]
    fn a_flow_with_no_file_contexts_persists_no_snapshot_manifest_directory_or_log_event() {
        let dir = std::env::temp_dir().join(format!(
            "sfh-snap-persist-empty-{}",
            contain::random_nonce()
        ));
        let flow_dir = dir.join("flow");
        let run_dir = dir.join("run");
        contain::mkdir_private(&flow_dir).unwrap();
        contain::mkdir_private(&run_dir).unwrap();
        // No `contexts:` key whatsoever - `#[serde(default)]` gives `flow.contexts`
        // an empty map, exactly like a flow author who never wrote the block.
        let flow: flow::Flow =
            serde_yaml_ng::from_str("name: t\nsteps:\n  - id: a\n    cmd: [\"echo\", \"x\"]\n")
                .expect("test flow parses");
        let containment = context::Containment {
            flow_dir: &flow_dir,
            workspace: None,
        };
        let mut log = std::fs::File::create(run_dir.join("log.jsonl")).unwrap();

        let registry =
            snapshot_and_persist_context(&flow, &containment, &run_dir, &mut log).unwrap();

        assert!(registry.is_empty());
        assert!(
            !run_dir.join(CONTEXT_SNAPSHOT_MANIFEST).exists(),
            "a flow with nothing to pin must not leave a manifest naming zero sources"
        );
        assert!(
            !run_dir.join(CONTEXT_SNAPSHOT_DIR).exists(),
            "a flow with nothing to pin must not create the snapshot directory either"
        );
        drop(log);
        let events: Vec<String> = std::fs::read_to_string(run_dir.join("log.jsonl"))
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v.get("event").and_then(|e| e.as_str()).map(str::to_string))
            .collect();
        assert!(
            !events.contains(&"context_snapshot".to_string()),
            "no event should be logged for a snapshot that pinned nothing: {events:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other half of the test above: a flow that DOES declare a `kind:
    /// file` context must keep getting the manifest, the snapshot directory
    /// and the log event exactly as before - this fix only removes the
    /// artifacts for a run with nothing to pin, never for one that has
    /// something.
    #[test]
    fn a_flow_with_a_file_context_still_persists_the_manifest_directory_and_log_event() {
        let dir = std::env::temp_dir().join(format!(
            "sfh-snap-persist-nonempty-{}",
            contain::random_nonce()
        ));
        let flow_dir = dir.join("flow");
        let run_dir = dir.join("run");
        contain::mkdir_private(&flow_dir).unwrap();
        contain::mkdir_private(&run_dir).unwrap();
        std::fs::write(flow_dir.join("TASK.md"), "do the thing").unwrap();
        let flow = context_flow("  task:\n    file: \"TASK.md\"\n");
        let containment = context::Containment {
            flow_dir: &flow_dir,
            workspace: None,
        };
        let mut log = std::fs::File::create(run_dir.join("log.jsonl")).unwrap();

        let registry =
            snapshot_and_persist_context(&flow, &containment, &run_dir, &mut log).unwrap();

        assert_eq!(registry.len(), 1);
        assert!(
            run_dir.join(CONTEXT_SNAPSHOT_MANIFEST).exists(),
            "a flow that names a file context must still get a manifest"
        );
        assert!(
            run_dir.join(CONTEXT_SNAPSHOT_DIR).is_dir(),
            "the frozen copy must still land in the snapshot directory"
        );
        drop(log);
        let events: Vec<String> = std::fs::read_to_string(run_dir.join("log.jsonl"))
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v.get("event").and_then(|e| e.as_str()).map(str::to_string))
            .collect();
        assert!(
            events.contains(&"context_snapshot".to_string()),
            "the run log must still record what this run pinned: {events:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_resumed_run_keeps_reading_the_original_snapshot_even_after_the_live_file_changes_before_the_resume(
    ) {
        // The resume-semantics decision: --resume (and --force-resume) never
        // re-capture. A resumed run reads back exactly what the FIRST attempt
        // pinned, so remaining steps see the same definition of the work the
        // already-completed steps did - never a mix of the two.
        let dir = std::env::temp_dir().join(format!("sfh-snap-resume-{}", contain::random_nonce()));
        let flow_dir = dir.join("flow");
        let run_dir = dir.join("run");
        contain::mkdir_private(&flow_dir).unwrap();
        contain::mkdir_private(&run_dir).unwrap();
        let task = flow_dir.join("TASK.md");
        std::fs::write(&task, "v1: what the first attempt saw").unwrap();
        let flow = context_flow("  task:\n    file: \"TASK.md\"\n");
        let containment = context::Containment {
            flow_dir: &flow_dir,
            workspace: None,
        };

        // ---- the original (non-resumed) attempt ----
        let snap = snapshot_file_contexts(&flow, &containment, &run_dir).unwrap();
        let manifest_text = serde_json::to_string_pretty(&json!({
            "schema_version": 1,
            "sources": snap.sources,
        }))
        .unwrap();
        contain::write_private_atomic(&run_dir.join(CONTEXT_SNAPSHOT_MANIFEST), manifest_text)
            .unwrap();

        // Something outside the run edits TASK.md before the run is resumed.
        std::fs::write(&task, "v2: edited after the crash, before the resume").unwrap();

        // ---- the resumed attempt ----
        let registry = match load_context_snapshot(&run_dir).unwrap() {
            ResumedSnapshot::Loaded(map) => map,
            _ => panic!("expected a loaded snapshot"),
        };
        let _guard = context::activate_snapshot(registry);
        let mut r = |_: &str| -> Result<String, String> { Ok(String::new()) };
        let bundle =
            context::build(&flow, &["task".to_string()], &containment, None, &mut r).unwrap();
        assert!(
            bundle.text.contains("v1: what the first attempt saw"),
            "{}",
            bundle.text
        );
        assert!(!bundle.text.contains("v2:"), "{}", bundle.text);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_leaf_run_claim_obeys_the_total_limit() {
        let mut total = 0;
        claim_leaf_runs(&mut total, 1, 1, "primary").unwrap();
        let error = claim_leaf_runs(&mut total, 1, 1, "fallback").unwrap_err();
        assert!(error.contains("max_total_steps (1)"), "{error}");
        assert_eq!(total, 1, "a rejected claim must not mutate the count");
    }

    #[test]
    fn run_cost_saturates_and_never_refunds_or_leaves_json_space() {
        let mut total = 0.25;
        accumulate_cost(&mut total, -1.0);
        assert_eq!(total, 0.25);
        accumulate_cost(&mut total, f64::MAX);
        accumulate_cost(&mut total, f64::MAX);
        assert_eq!(total, f64::MAX);
        assert!(total.is_finite());
    }

    #[test]
    fn resume_refuses_a_visit_counter_that_cannot_advance() {
        let dir =
            std::env::temp_dir().join(format!("sfh-visit-overflow-{}", contain::random_nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        let visit = u32::MAX;
        let events = [
            json!({"event":"group_start","step":"fan","visit":visit,"children":1}),
            json!({
                "event":"step_end",
                "step":"member",
                "parent":"fan",
                "visit":visit,
                "exit":0,
                "timed_out":false,
                "interrupted":false
            }),
            json!({
                "event":"aggregate_end",
                "step":"fan",
                "visit":visit,
                "exit":1,
                "failed":true
            }),
        ];
        let log = events
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.join("log.jsonl"), log).unwrap();

        let error = match load_resume(&dir) {
            Err(error) => error,
            Ok(_) => panic!("the visit counter must not wrap back to zero"),
        };
        assert!(
            error.contains("exhausted the supported visit counter"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_refuses_a_named_result_artifact_that_is_missing() {
        let dir =
            std::env::temp_dir().join(format!("sfh-missing-chain-{}", contain::random_nonce()));
        contain::mkdir_private(&dir).unwrap();
        let event = json!({
            "event":"step_end",
            "step":"paid",
            "visit":1,
            "exit":0,
            "timed_out":false,
            "interrupted":false,
            "chain_file":"paid.chain.txt"
        });
        std::fs::write(dir.join("log.jsonl"), event.to_string()).unwrap();

        let error = match load_resume(&dir) {
            Err(error) => error,
            Ok(_) => panic!("a durable checkpoint must not silently restore a missing chain"),
        };
        assert!(
            error.contains("names missing artifact 'paid.chain.txt'"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_verifies_the_output_bytes_named_by_a_checkpoint() {
        let dir = std::env::temp_dir().join(format!("sfh-output-hash-{}", contain::random_nonce()));
        contain::mkdir_private(&dir).unwrap();
        std::fs::write(dir.join("paid.chain.txt"), "changed after completion").unwrap();
        let event = json!({
            "event":"step_end",
            "step":"paid",
            "visit":1,
            "exit":0,
            "timed_out":false,
            "interrupted":false,
            "chain_file":"paid.chain.txt",
            "output_hash":fingerprint("the completed output")
        });
        std::fs::write(dir.join("log.jsonl"), event.to_string()).unwrap();

        let error = match load_resume(&dir) {
            Err(error) => error,
            Ok(_) => panic!("changed output must not be routed as the recorded result"),
        };
        assert!(
            error.contains("does not match the recorded output hash"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_refuses_to_repeat_an_uncertain_paid_persistence_failure() {
        let dir =
            std::env::temp_dir().join(format!("sfh-persist-failed-{}", contain::random_nonce()));
        contain::mkdir_private(&dir).unwrap();
        let log = [
            json!({"event":"step_start","step":"paid","visit":1,"cmd":"provider"}),
            json!({
                "event":"persistence_failure",
                "step":"paid",
                "visit":1,
                "cost_usd":0.25,
                "error":"cannot persist required chain artifact"
            }),
        ]
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        std::fs::write(dir.join("log.jsonl"), log).unwrap();

        let error = match load_resume(&dir) {
            Err(error) => error,
            Ok(_) => panic!("the uncertain external side effect must not execute again"),
        };
        assert!(error.contains("run is non-resumable"), "{error}");
        assert!(error.contains("verify the external side effect"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn route(
        when_contains: Option<&str>,
        when_matches: Option<&str>,
        when_last_line_is: Option<&str>,
    ) -> flow::Route {
        flow::Route {
            when_contains: when_contains.map(String::from),
            when_matches: when_matches.map(String::from),
            when_last_line_contains: None,
            when_last_line_is: when_last_line_is.map(String::from),
            when_last_line_matches: None,
            when_exit: None,
            when_label_is: None,
            when_outcome_is: None,
            when_protocol_is: None,
            when_stderr_matches: None,
            when_members: None,
            goto: "end".to_string(),
        }
    }

    #[test]
    fn route_line_follows_what_the_rule_actually_judged() {
        // F4: a position event has to say what the rule was tested against.
        // A whole-text predicate is not judged on a line, so it records the head
        // of the routing text; last-line predicates and the catch-all record the
        // last line, which IS what they compared.
        let text = "first line\nlast line";
        assert_eq!(
            route_line_of(&route(None, None, Some("last line")), text, "last line"),
            "last line"
        );
        assert_eq!(
            route_line_of(&route(None, None, None), text, "last line"),
            "last line",
            "the catch-all records the last line too"
        );
        assert_eq!(
            route_line_of(&route(Some("first"), None, None), text, "last line"),
            text
        );
        assert_eq!(
            route_line_of(&route(None, Some("^first"), None), text, "last line"),
            text
        );
        // A rule that mixes both is judged on the whole text as well.
        assert_eq!(
            route_line_of(
                &route(Some("first"), None, Some("last line")),
                text,
                "last line"
            ),
            text
        );
    }

    /// A one-step flow plus the outputs entry the engine would have inserted
    /// for it just before routing, so `evaluate_route` can be exercised without
    /// running anything.
    fn route_probe(
        route_yaml: &str,
        out: template::StepOutput,
    ) -> (
        flow::Flow,
        BTreeMap<String, template::StepOutput>,
        HashSet<String>,
    ) {
        let flow: flow::Flow = serde_yaml_ng::from_str(&format!(
            "name: t\nsteps:\n  - id: probe\n    cmd: [\"echo\", \"x\"]\n    route:\n{route_yaml}"
        ))
        .expect("test flow parses");
        let mut outputs = BTreeMap::new();
        outputs.insert("probe".to_string(), out);
        let ids: HashSet<String> = flow.steps.iter().map(|s| s.id.clone()).collect();
        (flow, outputs, ids)
    }

    fn probe_output(exit: i32, stderr_file: &str) -> template::StepOutput {
        template::StepOutput {
            output: "PROBE-DONE".into(),
            outputs: "PROBE-DONE".into(),
            output_file: String::new(),
            exit,
            stderr_file: stderr_file.to_string(),
            outcome: String::new(),
            label: String::new(),
        }
    }

    #[test]
    fn when_exit_compares_the_steps_own_normalized_exit() {
        // F6: the gate an `on_error: continue` probe needs. `when_exit: 0` on a
        // probe that was SUPPOSED to be refused is how "the guard is gone" gets
        // caught, so the equality has to be exact - not "non-zero".
        let rules = "      - {when_exit: 0, goto: leaked}\n      - {when_exit: 3, goto: expected}\n      - {goto: other}\n";
        let vars = BTreeMap::new();
        for (exit, want, rule) in [(3, "expected", 1), (0, "leaked", 0), (9, "other", 2)] {
            let (flow, outputs, ids) = route_probe(rules, probe_output(exit, ""));
            let ctx = template::Ctx {
                vars: &vars,
                outputs: &outputs,
                step_ids: &ids,
                builtins: BTreeMap::new(),
            };
            let hit = evaluate_route(
                &flow.steps[0],
                "PROBE-DONE",
                &ctx,
                Path::new("."),
                Members::NotAGroup,
                Some(protocol::ProtocolState::Plain),
            )
            .unwrap()
            .expect("some rule always matches here");
            assert_eq!(hit.goto, want, "exit {exit}");
            assert_eq!(hit.rule, rule, "exit {exit}");
        }
        // A when_exit rule is a RULE, not a catch-all: the position event has to
        // say the flow branched on a condition.
        let (flow, outputs, ids) = route_probe(rules, probe_output(3, ""));
        let ctx = template::Ctx {
            vars: &vars,
            outputs: &outputs,
            step_ids: &ids,
            builtins: BTreeMap::new(),
        };
        let hit = evaluate_route(
            &flow.steps[0],
            "PROBE-DONE",
            &ctx,
            Path::new("."),
            Members::NotAGroup,
            Some(protocol::ProtocolState::Plain),
        )
        .unwrap()
        .unwrap();
        assert!(matches!(hit.via, PositionVia::Rule));
    }

    #[test]
    fn when_exit_fails_closed_without_a_recorded_output() {
        // No outputs entry = no exit to compare. That is missing evidence, so
        // the rule must not fire (the catch-all below it does).
        let (flow, _outputs, ids) = route_probe(
            "      - {when_exit: 0, goto: matched}\n      - {goto: other}\n",
            probe_output(0, ""),
        );
        let vars = BTreeMap::new();
        let empty = BTreeMap::new();
        let ctx = template::Ctx {
            vars: &vars,
            outputs: &empty,
            step_ids: &ids,
            builtins: BTreeMap::new(),
        };
        let hit = evaluate_route(
            &flow.steps[0],
            "PROBE-DONE",
            &ctx,
            Path::new("."),
            Members::NotAGroup,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.goto, "other");
    }

    #[test]
    fn when_protocol_is_uses_recorded_evidence_and_fails_closed_without_it() {
        let rules = "      - {when_protocol_is: invalid, goto: salvage}\n      - {goto: fail}\n";
        let (flow, outputs, ids) = route_probe(rules, probe_output(1, ""));
        let vars = BTreeMap::new();
        let ctx = template::Ctx {
            vars: &vars,
            outputs: &outputs,
            step_ids: &ids,
            builtins: BTreeMap::new(),
        };
        let hit = evaluate_route(
            &flow.steps[0],
            "broken adapter output",
            &ctx,
            Path::new("."),
            Members::NotAGroup,
            Some(protocol::ProtocolState::Invalid),
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.goto, "salvage");
        assert_eq!(hit.protocol, Some(protocol::ProtocolState::Invalid));

        let missing = evaluate_route(
            &flow.steps[0],
            "broken adapter output",
            &ctx,
            Path::new("."),
            Members::NotAGroup,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(missing.goto, "fail");
    }

    #[test]
    fn when_stderr_matches_reads_the_file_and_fails_closed_when_it_is_gone() {
        // F6: the predicate judges <id>.err.txt, in live runs as well as
        // resumed ones, so a deleted artifact is "no evidence" - never a pass.
        let dir = std::env::temp_dir().join(format!("sfh-f6-stderr-{}", contain::random_nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = dir.join("probe.err.txt");
        std::fs::write(&err, "sfh: refusing to resume: no recorded access level\n").unwrap();
        let rules = "      - {when_stderr_matches: \"refusing to resume\", goto: guard_fired}\n      - {goto: broken}\n";
        let vars = BTreeMap::new();
        let (flow, outputs, ids) = route_probe(rules, probe_output(3, &err.display().to_string()));
        let ctx = template::Ctx {
            vars: &vars,
            outputs: &outputs,
            step_ids: &ids,
            builtins: BTreeMap::new(),
        };
        let hit = evaluate_route(
            &flow.steps[0],
            "PROBE-DONE",
            &ctx,
            &dir,
            Members::NotAGroup,
            Some(protocol::ProtocolState::Plain),
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.goto, "guard_fired");

        std::fs::remove_file(&err).unwrap();
        let hit = evaluate_route(
            &flow.steps[0],
            "PROBE-DONE",
            &ctx,
            &dir,
            Members::NotAGroup,
            Some(protocol::ProtocolState::Plain),
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.goto, "broken", "a missing stderr file must not match");

        // A step that never recorded a stderr file at all (a fan-out group)
        // has nothing to judge either.
        let (flow, outputs, ids) = route_probe(rules, probe_output(3, ""));
        let ctx = template::Ctx {
            vars: &vars,
            outputs: &outputs,
            step_ids: &ids,
            builtins: BTreeMap::new(),
        };
        let hit = evaluate_route(
            &flow.steps[0],
            "PROBE-DONE",
            &ctx,
            &dir,
            Members::NotAGroup,
            Some(protocol::ProtocolState::Plain),
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.goto, "broken");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn route_line_is_clipped_by_characters_not_bytes() {
        // A step is free to end in one enormous line; the log is not the place
        // to keep it. Clipping counts characters, so a multi-byte last line
        // cannot be cut in the middle of one.
        let long: String = "ab".repeat(400);
        assert_eq!(
            route_line_of(&route(None, None, None), &long, &long).len(),
            200
        );
        let wide: String = "日".repeat(400);
        let cut = route_line_of(&route(None, None, None), &wide, &wide);
        assert_eq!(cut.chars().count(), 200);
        assert_eq!(cut.len(), 600, "each kept char must still be whole");
    }

    #[test]
    fn resume_restores_recorded_access_and_fails_closed_on_bogus() {
        // rev_complete S2-4 / rev_break #11: the access a step ran under is read
        // back from log.jsonl on --resume. A valid value restores; a bogus one
        // (an edited log) must restore as None so the escalation guard refuses
        // it, and a missing one is likewise None.
        let dir =
            std::env::temp_dir().join(format!("sfh-resume-access-{}", contain::random_nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        // Real step_end lines always carry timed_out/interrupted (log_step_end
        // writes them); with rev_break #15 a line missing them is not "ok", so
        // include them here to exercise access restoration on a clean success.
        let log = concat!(
            "{\"event\":\"step_end\",\"step\":\"good\",\"visit\":1,\"exit\":0,\"timed_out\":false,\"interrupted\":false,\"session\":{\"tool\":\"claude\",\"id\":\"s1\",\"access\":\"read\"}}\n",
            "{\"event\":\"step_end\",\"step\":\"bogus\",\"visit\":1,\"exit\":0,\"timed_out\":false,\"interrupted\":false,\"session\":{\"tool\":\"claude\",\"id\":\"s2\",\"access\":\"bogus\"}}\n",
            "{\"event\":\"step_end\",\"step\":\"missing\",\"visit\":1,\"exit\":0,\"timed_out\":false,\"interrupted\":false,\"session\":{\"tool\":\"claude\",\"id\":\"s3\"}}\n",
        );
        std::fs::write(dir.join("log.jsonl"), log).unwrap();

        let st = load_resume(&dir).expect("load_resume");
        assert_eq!(
            st.sessions.get("good").and_then(|s| s.access),
            Some(preset::Access::Read),
            "a valid recorded access must restore"
        );
        assert_eq!(
            st.sessions.get("bogus").and_then(|s| s.access),
            None,
            "an unparseable recorded access must restore as None (fail closed)"
        );
        assert_eq!(
            st.sessions.get("missing").and_then(|s| s.access),
            None,
            "an absent recorded access must restore as None"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_treats_missing_exit_as_failure() {
        // rev_break #12: a step_end with no integer `exit` (an edited log) must
        // not read back as a clean success. Without a positive exit 0 the step
        // is not "ok", so it is not recorded as the last success.
        let dir = std::env::temp_dir().join(format!("sfh-resume-exit-{}", contain::random_nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("log.jsonl"),
            "{\"event\":\"step_end\",\"step\":\"a\",\"visit\":1}\n",
        )
        .unwrap();
        let st = load_resume(&dir).expect("load_resume");
        assert_eq!(
            st.last_success, None,
            "a step with no exit must not be a success"
        );
        assert_eq!(
            st.outputs.get("a").map(|o| o.exit),
            Some(1),
            "an absent exit restores as a failure code"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_replays_failed_on_error_control_without_reprobing() {
        let dir =
            std::env::temp_dir().join(format!("sfh-resume-failed-{}", contain::random_nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("probe.chain.txt"), "guard refused\n").unwrap();
        std::fs::write(dir.join("probe.out.txt"), "guard refused\n").unwrap();
        std::fs::write(dir.join("probe.err.txt"), "permission denied\n").unwrap();
        std::fs::write(
            dir.join("log.jsonl"),
            "{\"event\":\"step_end\",\"step\":\"probe\",\"visit\":1,\"exit\":3,\"timed_out\":false,\"interrupted\":false,\"protocol_state\":\"invalid\",\"chain_file\":\"probe.chain.txt\",\"out_file\":\"probe.out.txt\"}\n",
        )
        .unwrap();

        let continuing: flow::Flow = serde_yaml_ng::from_str(
            "name: t\nsteps:\n  - id: probe\n    cmd: echo probe\n    on_error: continue\n  - id: next\n    cmd: echo next\n",
        )
        .unwrap();
        let resumed =
            load_resume_for_flow(&dir, Some(&continuing)).expect("load failed probe for resume");
        let pending = resumed
            .pending_route
            .as_ref()
            .expect("on_error control is the only missing action");
        assert!(pending.errored);
        assert_eq!(pending.step, "probe");
        assert_eq!(pending.route_text, "guard refused");
        assert_eq!(pending.protocol, Some(protocol::ProtocolState::Invalid));
        assert_eq!(resumed.outputs["probe"].exit, 3);
        assert!(
            resumed.outputs["probe"].output.contains("did not complete"),
            "downstream output must retain the failure banner"
        );

        // An ordinary failed step intentionally remains retryable on resume.
        let retrying: flow::Flow =
            serde_yaml_ng::from_str("name: t\nsteps:\n  - id: probe\n    cmd: echo probe\n")
                .unwrap();
        let resumed =
            load_resume_for_flow(&dir, Some(&retrying)).expect("load retryable failed probe");
        assert!(resumed.pending_route.is_none());
        assert_eq!(resumed.start.as_deref(), Some("probe"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_continues_the_checkpointed_fallback_in_the_same_visit() {
        let dir =
            std::env::temp_dir().join(format!("sfh-resume-fallback-{}", contain::random_nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("work.chain.txt"), "primary failed\n").unwrap();
        std::fs::write(dir.join("work.out.txt"), "").unwrap();
        std::fs::write(dir.join("work.err.txt"), "failed\n").unwrap();
        let checkpoint = "{\"event\":\"step_end\",\"step\":\"work\",\"visit\":1,\
            \"exit\":7,\"timed_out\":false,\"interrupted\":false,\"attempts\":1,\
            \"dur_ms\":10,\"cost_usd\":0.25,\"chain_file\":\"work.chain.txt\",\
            \"out_file\":\"work.out.txt\",\"next_fallback\":\"backup\"}\n";
        std::fs::write(dir.join("log.jsonl"), checkpoint).unwrap();

        let state = load_resume(&dir).expect("fallback checkpoint must be resumable");
        let unfinished = state
            .unfinished_step
            .as_ref()
            .expect("the selected fallback is unfinished");
        assert_eq!(state.start.as_deref(), Some("work"));
        assert_eq!(unfinished.step, "work");
        assert_eq!(unfinished.visit, 1);
        assert_eq!(unfinished.profile.as_deref(), Some("backup"));
        assert_eq!(state.visits.get("work"), Some(&1));
        assert_eq!(state.total, 1);
        assert_eq!(state.cost_usd, 0.25);
        assert!(state.pending_route.is_none());

        std::fs::write(dir.join("work.fb-backup.chain.txt"), "recovered\n").unwrap();
        std::fs::write(dir.join("work.fb-backup.out.txt"), "recovered\n").unwrap();
        std::fs::write(dir.join("work.fb-backup.err.txt"), "").unwrap();
        let completed = "{\"event\":\"step_end\",\"step\":\"work\",\"visit\":1,\
            \"exit\":0,\"timed_out\":false,\"interrupted\":false,\"attempts\":1,\
            \"dur_ms\":5,\"cost_usd\":0.10,\"chain_file\":\"work.fb-backup.chain.txt\",\
            \"out_file\":\"work.fb-backup.out.txt\",\"next_fallback\":null}\n";
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("log.jsonl"))
            .unwrap();
        write!(log, "{completed}").unwrap();
        drop(log);

        let state = load_resume(&dir).expect("completed fallback must close the checkpoint");
        assert!(state.unfinished_step.is_none());
        assert_eq!(state.last_success.as_deref(), Some("work"));
        assert_eq!(state.total, 2);
        assert_eq!(state.cost_usd, 0.35);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_preserves_retry_budget_landing_until_its_event_is_durable() {
        let dir = std::env::temp_dir().join(format!(
            "sfh-resume-retry-budget-{}",
            contain::random_nonce()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let checkpoint = concat!(
            "{\"event\":\"step_end\",\"step\":\"work\",\"visit\":1,",
            "\"exit\":7,\"timed_out\":false,\"interrupted\":false,",
            "\"retry_budget_exhausted\":true}\n"
        );
        std::fs::write(dir.join("log.jsonl"), checkpoint).unwrap();

        let state = load_resume(&dir).expect("retry landing checkpoint must load");
        assert_eq!(state.start.as_deref(), Some("work"));
        assert_eq!(state.retry_budget_trigger.as_deref(), Some("wall_clock"));

        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("log.jsonl"))
            .unwrap();
        writeln!(
            log,
            "{{\"event\":\"budget_landing\",\"trigger\":\"wall_clock\",\"goto\":\"wrap\"}}"
        )
        .unwrap();
        drop(log);

        let state = load_resume(&dir).expect("durable landing must load");
        assert!(state.budget_landed);
        assert!(state.retry_budget_trigger.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_keeps_a_fanout_members_fallback_checkpoint() {
        let dir = std::env::temp_dir().join(format!(
            "sfh-resume-member-fallback-{}",
            contain::random_nonce()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("kid.chain.txt"), "primary failed\n").unwrap();
        std::fs::write(dir.join("kid.out.txt"), "").unwrap();
        std::fs::write(dir.join("kid.err.txt"), "failed\n").unwrap();
        let log = concat!(
            "{\"event\":\"group_start\",\"step\":\"fan\",\"visit\":2,\"children\":1}\n",
            "{\"event\":\"step_end\",\"step\":\"kid\",\"parent\":\"fan\",\"visit\":2,",
            "\"exit\":7,\"timed_out\":false,\"interrupted\":false,\"attempts\":1,",
            "\"dur_ms\":10,\"cost_usd\":0.25,\"chain_file\":\"kid.chain.txt\",",
            "\"out_file\":\"kid.out.txt\",\"next_fallback\":\"backup\"}\n",
        );
        std::fs::write(dir.join("log.jsonl"), log).unwrap();

        let state = load_resume(&dir).expect("member fallback checkpoint must load");
        assert_eq!(state.start.as_deref(), Some("fan"));
        assert_eq!(
            state
                .member_fallbacks
                .get(&("fan".into(), 2, "kid".into()))
                .map(String::as_str),
            Some("backup")
        );
        assert_eq!(state.total, 1);
        assert_eq!(state.cost_usd, 0.25);

        std::fs::write(dir.join("kid.fb-backup.chain.txt"), "recovered\n").unwrap();
        std::fs::write(dir.join("kid.fb-backup.out.txt"), "recovered\n").unwrap();
        std::fs::write(dir.join("kid.fb-backup.err.txt"), "").unwrap();
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("log.jsonl"))
            .unwrap();
        writeln!(
            log,
            "{{\"event\":\"step_end\",\"step\":\"kid\",\"parent\":\"fan\",\"visit\":2,\
             \"exit\":0,\"timed_out\":false,\"interrupted\":false,\"attempts\":1,\
             \"dur_ms\":5,\"cost_usd\":0.10,\"chain_file\":\"kid.fb-backup.chain.txt\",\
             \"out_file\":\"kid.fb-backup.out.txt\",\"next_fallback\":null}}"
        )
        .unwrap();
        drop(log);

        let state = load_resume(&dir).expect("completed member fallback must load");
        assert!(state.member_fallbacks.is_empty());
        assert!(state
            .completed_members
            .get(&("fan".into(), 2))
            .is_some_and(|members| members.contains("kid")));
        assert_eq!(state.total, 2);
        assert_eq!(state.cost_usd, 0.35);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_keeps_postprocessing_pending_until_its_end_marker() {
        let dir = std::env::temp_dir().join(format!(
            "sfh-resume-postprocess-{}",
            contain::random_nonce()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("source.chain.txt"), "large original\n").unwrap();
        std::fs::write(dir.join("source.out.txt"), "large original\n").unwrap();
        std::fs::write(dir.join("source.err.txt"), "").unwrap();
        let completion = "{\"event\":\"step_end\",\"step\":\"source\",\"visit\":1,\
            \"exit\":0,\"timed_out\":false,\"interrupted\":false,\"attempts\":1,\
            \"dur_ms\":10,\"chain_file\":\"source.chain.txt\",\
            \"out_file\":\"source.out.txt\",\"postprocess_pending\":true}\n";
        std::fs::write(dir.join("log.jsonl"), completion).unwrap();

        let state = load_resume(&dir).expect("post-processing checkpoint must be resumable");
        let pending = state
            .pending_route
            .as_ref()
            .expect("the completed step still has a route to evaluate");
        assert!(pending.postprocess);
        assert!(!pending.compact_done);
        assert!(!pending.notes_done);
        assert_eq!(pending.step, "source");
        assert_eq!(pending.visit, 1);
        assert_eq!(state.total, 1);

        std::fs::write(dir.join("source.precompact.txt"), "large original\n").unwrap();
        std::fs::write(dir.join("source.chain.txt"), "small summary\n").unwrap();
        let marker = note_marker("source", 1, "large original");
        std::fs::write(
            dir.join("notes.md"),
            format!("{marker}\n## source (visit 1)\nlarge original\n\n"),
        )
        .unwrap();
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("log.jsonl"))
            .unwrap();
        writeln!(
            log,
            "{}",
            json!({
                "event": "compact_end",
                "step": "source",
                "visit": 1,
                "chars": 13,
                "cost_usd": 0.25,
                "precompact_file": "source.precompact.txt",
                "chain_file": "source.chain.txt",
            })
        )
        .unwrap();
        writeln!(
            log,
            "{}",
            json!({
                "event": "notes_end",
                "step": "source",
                "visit": 1,
                "marker": marker,
            })
        )
        .unwrap();
        drop(log);

        let state = load_resume(&dir).expect("post-processing substages must be resumable");
        let pending = state.pending_route.as_ref().unwrap();
        assert!(pending.postprocess);
        assert!(pending.compact_done);
        assert!(pending.notes_done);
        assert_eq!(state.total, 2);
        assert_eq!(state.cost_usd, 0.25);
        assert_eq!(state.outputs["source"].output, "small summary");
        assert_eq!(state.outputs["source"].outputs, "large original");

        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("log.jsonl"))
            .unwrap();
        writeln!(
            log,
            "{{\"event\":\"postprocess_end\",\"step\":\"source\",\"visit\":1}}"
        )
        .unwrap();
        drop(log);

        let state = load_resume(&dir).expect("postprocess_end must close the checkpoint");
        assert!(
            !state.pending_route.as_ref().unwrap().postprocess,
            "routing may replay only after post-processing is durable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compact_start_recovers_the_original_across_publish_before_event() {
        let dir = std::env::temp_dir().join(format!(
            "sfh-resume-compact-start-{}",
            contain::random_nonce()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("source.out.txt"), "large original\n").unwrap();
        // Simulate an atomic compacted-chain publish followed by a kill before
        // compact_end. The earlier step_end now resolves to these newer bytes.
        std::fs::write(dir.join("source.chain.txt"), "uncheckpointed summary\n").unwrap();
        std::fs::write(dir.join("source.precompact.txt"), "large original\n").unwrap();
        std::fs::write(
            dir.join("log.jsonl"),
            concat!(
                "{\"event\":\"step_end\",\"step\":\"source\",\"visit\":1,",
                "\"exit\":0,\"timed_out\":false,\"interrupted\":false,\"attempts\":1,",
                "\"dur_ms\":10,\"chain_file\":\"source.chain.txt\",",
                "\"out_file\":\"source.out.txt\",\"postprocess_pending\":true}\n",
                "{\"event\":\"compact_start\",\"step\":\"source\",\"visit\":1,",
                "\"precompact_file\":\"source.precompact.txt\"}\n"
            ),
        )
        .unwrap();

        let state = load_resume(&dir).expect("compact_start must be recoverable");
        let pending = state.pending_route.as_ref().unwrap();
        assert!(pending.postprocess);
        assert!(!pending.compact_done);
        assert_eq!(pending.route_text, "large original");
        assert_eq!(state.outputs["source"].output, "large original");
        assert_eq!(state.outputs["source"].outputs, "large original");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_records_completed_fanout_members_under_parent_and_visit() {
        // F-2: a crash mid-fan-out leaves step_end events for the members that
        // already finished. load_resume must hand them back keyed by (parent
        // group id, visit) under the member's own log label - a parallel child
        // id or a foreach "id[i]" - which is exactly what the group re-derives
        // on resume, so it skips those members instead of executing them a
        // second time (double billing, duplicate sessions, and a restored
        // count plus a full fresh batch that wedges max_total_steps).
        let dir =
            std::env::temp_dir().join(format!("sfh-resume-members-{}", contain::random_nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("log.jsonl"),
            "{\"event\":\"group_start\",\"step\":\"fan\",\"visit\":1,\"children\":2}\n\
             {\"event\":\"step_end\",\"step\":\"fa\",\"parent\":\"fan\",\"visit\":1,\"exit\":0,\"timed_out\":false,\"interrupted\":false,\"chain_file\":\"fa.chain.txt\",\"out_file\":\"fa.out.txt\"}\n\
             {\"event\":\"foreach_start\",\"step\":\"each\",\"visit\":2,\"items\":2}\n\
             {\"event\":\"step_end\",\"step\":\"each[0]\",\"parent\":\"each\",\"visit\":2,\"exit\":0,\"timed_out\":false,\"interrupted\":false,\"chain_file\":\"each.i0.chain.txt\",\"out_file\":\"each.i0.out.txt\"}\n",
        )
        .unwrap();
        std::fs::write(dir.join("fa.chain.txt"), "FA-A\n").unwrap();
        std::fs::write(dir.join("fa.out.txt"), "FA-A\n").unwrap();
        std::fs::write(dir.join("each.i0.chain.txt"), "item-alpha\n").unwrap();
        std::fs::write(dir.join("each.i0.out.txt"), "item-alpha\n").unwrap();

        let st = load_resume(&dir).expect("load_resume");
        let fan = st
            .completed_members
            .get(&("fan".to_string(), 1))
            .cloned()
            .unwrap_or_default();
        assert!(
            fan.contains("fa"),
            "a completed parallel child must be recorded under its parent group"
        );
        assert!(
            !fan.contains("fb"),
            "an unstarted sibling must not be marked completed"
        );
        let each = st
            .completed_members
            .get(&("each".to_string(), 2))
            .cloned()
            .unwrap_or_default();
        assert!(
            each.contains("each[0]"),
            "a completed foreach item must be recorded under its parent step and visit"
        );
        assert!(
            st.outputs.contains_key("fa") && st.outputs.contains_key("each[0]"),
            "the recorded outputs the skip lives on must be restored too"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn verdicts(spec: &[(&str, bool, &str)]) -> Vec<MemberVerdict> {
        spec.iter()
            .map(|(id, ok, line)| MemberVerdict::new(id, *ok, i32::from(!*ok), line))
            .collect()
    }

    fn quantifier(at_least: Option<u32>, all: Option<bool>) -> flow::WhenMembers {
        flow::WhenMembers {
            last_line_is: "YES".into(),
            at_least,
            all,
        }
    }

    #[test]
    fn a_member_vote_needs_both_a_clean_exit_and_the_exact_line() {
        // F1: the two halves of a vote. The failing member here says exactly
        // the right words - which is the whole point, because in the group's
        // routing text that is all anyone can see.
        let ms = verdicts(&[("a", true, "YES"), ("b", true, "YES"), ("c", false, "YES")]);
        let m = Members::Known(&ms);
        let (votes, voters) = m
            .tally(&quantifier(Some(2), None), "YES", "fan", 0)
            .unwrap()
            .expect("two clean members agreed");
        assert_eq!(votes, 2);
        assert_eq!(voters, vec!["a".to_string(), "b".to_string()]);
        assert!(
            m.tally(&quantifier(Some(3), None), "YES", "fan", 0)
                .unwrap()
                .is_none(),
            "the member that said the words and failed must not make three"
        );
        assert!(
            m.tally(&quantifier(None, Some(true)), "YES", "fan", 0)
                .unwrap()
                .is_none(),
            "all: true cannot hold while one member failed"
        );
        // Only the member's OWN last line counts, and only in full.
        assert!(
            m.tally(&quantifier(Some(1), None), "YE", "fan", 0)
                .unwrap()
                .is_none(),
            "a prefix of the verdict is not the verdict"
        );
    }

    #[test]
    fn an_empty_fan_out_never_agrees() {
        // Invariant 6. "All of nothing" is true in logic, and a foreach that
        // produced zero workers has decided nothing - the one reading that
        // would let a gate open on silence.
        let none: [MemberVerdict; 0] = [];
        let m = Members::Known(&none);
        assert!(m
            .tally(&quantifier(None, Some(true)), "YES", "each", 0)
            .unwrap()
            .is_none());
        assert!(m
            .tally(&quantifier(Some(1), None), "YES", "each", 0)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_route_that_cannot_count_refuses_rather_than_guesses() {
        // Two different kinds of "no members", told apart by the caller: a leaf
        // step has none to count (validate refuses when_members there, so this
        // is defence only), while a fan-out whose record predates the snapshot
        // COULD have been counted and no longer can. Falling through to the
        // catch-all in the second case would make the branch depend on which
        // sfh wrote the run, silently.
        let wm = quantifier(Some(1), None);
        assert!(Members::NotAGroup
            .tally(&wm, "YES", "solo", 0)
            .unwrap()
            .is_none());
        let err = Members::Unrecorded
            .tally(&wm, "YES", "fan", 2)
            .expect_err("a fan-out with no record must not decide");
        assert!(err.contains("predates per-member route records"), "{err}");
        assert!(err.contains("route[2]"), "{err}");
    }

    #[test]
    fn a_restored_snapshot_is_read_whole_or_not_at_all() {
        // The denominator is the point. Dropping one unreadable entry and
        // counting the rest would SHRINK the group, and a smaller group is how
        // `all: true` starts passing on a fan-out that did not agree.
        let good = json!({"members": [
            {"id": "a", "ok": true, "exit": 0, "last_line": "YES"},
            {"id": "b", "ok": false, "exit": 1, "last_line": "YES"},
        ]});
        let ms = restored_members(&good).expect("a well-formed snapshot");
        assert_eq!(ms.len(), 2);
        assert!(ms[0].ok && !ms[1].ok);
        let torn = json!({"members": [
            {"id": "a", "ok": true, "exit": 0, "last_line": "YES"},
            {"id": "b", "ok": true, "exit": 0},
        ]});
        assert!(
            restored_members(&torn).is_none(),
            "one unreadable member must void the whole snapshot"
        );
        assert!(
            restored_members(&json!({"failed": false})).is_none(),
            "an aggregate_end from before the snapshot existed has none"
        );
    }

    #[test]
    fn a_recorded_vote_needs_its_two_halves_to_agree() {
        // log.jsonl is rewritable inside a run dir on --resume. Trusting `ok`
        // on its own would promote a member that exited 7 to a voter by editing
        // one word, so the exit code has to agree with it - the same reading
        // the step_end restore already uses.
        let forged = json!({"members": [{"id": "a", "ok": true, "exit": 7, "last_line": "YES"}]});
        let ms = restored_members(&forged).expect("well-formed, just contradictory");
        assert!(
            !ms[0].ok,
            "a member that exited 7 must not vote however the log labels it"
        );
    }

    #[test]
    fn a_recorded_verdict_line_is_trimmed_and_cut_the_same_way_live_and_restored() {
        // The tally compares this value, so live and resumed have to produce
        // identical bytes or a resume could pick a different branch.
        let crlf = MemberVerdict::new("a", true, 0, "prose\r\nVOTE-YES\r\n");
        assert_eq!(crlf.last_line, "VOTE-YES", "a CRLF trailer still votes");
        let long = MemberVerdict::new("a", true, 0, &"x".repeat(400));
        assert_eq!(long.last_line.chars().count(), ROUTE_LINE_CHARS);
        let restored = restored_members(
            &json!({"members": [{"id": "a", "ok": true, "exit": 0, "last_line": "x".repeat(400)}]}),
        )
        .expect("snapshot");
        assert_eq!(
            restored[0].last_line, long.last_line,
            "a hand-lengthened record must not become comparable to something live could not produce"
        );
        assert!(
            restored[0].clipped,
            "a record longer than the writer could have written is a cut line"
        );
    }

    #[test]
    fn a_cut_verdict_line_never_votes() {
        // The needle may be ROUTE_LINE_CHARS long itself - validate only refuses
        // LONGER ones - and a cut line is a prefix, so "equal after cutting"
        // would count a member that said the verdict and then kept talking. By
        // then the rest of the line is gone, so the honest answer is "cannot
        // tell", and invariant 6 puts that on the not-matching side.
        let needle = "A".repeat(ROUTE_LINE_CHARS);
        let overlong = MemberVerdict::new("a", true, 0, &"A".repeat(ROUTE_LINE_CHARS + 50));
        assert_eq!(overlong.last_line, needle, "the cut value compares equal");
        assert!(overlong.clipped);
        let exact = MemberVerdict::new("b", true, 0, &needle);
        assert!(!exact.clipped, "a line that fits whole was not cut");
        let ms = vec![overlong, exact];
        let m = Members::Known(&ms);
        let (votes, voters) = m
            .tally(&quantifier(Some(1), None), &needle, "fan", 0)
            .unwrap()
            .expect("the member whose line fits still votes");
        assert_eq!(votes, 1);
        assert_eq!(voters, vec!["b".to_string()]);
        assert!(
            m.tally(&quantifier(None, Some(true)), &needle, "fan", 0)
                .unwrap()
                .is_none(),
            "a cut line must not complete a unanimous vote"
        );
        // And the same answer after a resume, read back from the record.
        let restored = restored_members(&json!({"members": [
            {"id": "a", "ok": true, "exit": 0, "last_line": needle, "clipped": true},
            {"id": "b", "ok": true, "exit": 0, "last_line": needle},
        ]}))
        .expect("snapshot");
        let (votes, voters) = Members::Known(&restored)
            .tally(&quantifier(Some(1), None), &needle, "fan", 0)
            .unwrap()
            .expect("the unclipped member votes after a resume too");
        assert_eq!(votes, 1);
        assert_eq!(voters, vec!["b".to_string()]);
    }

    #[test]
    fn a_resumed_vote_is_counted_against_the_group_the_flow_declares() {
        // The snapshot satisfies `all: true` on its own terms whatever the flow
        // now says, so a group edited from 3 members to 4 (which needs
        // --force-resume, which is when this happens) would report unanimity on
        // a member that was never asked. Live and resumed must not disagree.
        let err = Members::Mismatch {
            recorded: 3,
            declared: 4,
        }
        .tally(&quantifier(None, Some(true)), "YES", "fan", 1)
        .expect_err("a snapshot of a different group cannot decide this one");
        assert!(err.contains('3') && err.contains('4'), "{err}");
        assert!(err.contains("route[1]"), "{err}");
    }

    #[test]
    fn gitignore_revalidation_accepts_only_an_effective_star() {
        // rev_complete S3-3: protect_runs_root writes the .gitignore and then
        // RE-READS it, refusing the run unless an effective `*` pattern is
        // present. This pins the predicate that drives that revalidation: only a
        // non-comment `*` (or `/*`) counts. A `*` inside a comment, or a narrower
        // pattern like `*.log`, would still let git track the rest and must read
        // as "not protected" so the run fails instead of proceeding unprotected.
        assert!(gitignore_ignores_everything("*\n"));
        assert!(gitignore_ignores_everything("# Created by sfh.\n*\n"));
        assert!(gitignore_ignores_everything("/*\n"));
        assert!(!gitignore_ignores_everything(""));
        assert!(!gitignore_ignores_everything("# *\n"));
        assert!(!gitignore_ignores_everything("*.log\n"));
        assert!(!gitignore_ignores_everything("# comment\n*.txt\n"));
    }

    #[test]
    fn splits_items_by_mode() {
        assert_eq!(split_items(" a \n\n b \n", None).unwrap(), vec!["a", "b"]);
        assert_eq!(
            split_items("prose [\"x\", \"y\"] trailing", Some("json")).unwrap(),
            vec!["x", "y"]
        );
        assert_eq!(
            split_items(
                "See reference [1]. Final answer: [\"x\", \"y\"]",
                Some("json")
            )
            .unwrap(),
            vec!["x", "y"],
            "a citation before the final array must not be joined to it"
        );
        assert_eq!(
            split_items(
                "draft [\"old\"]\nfinal [[\"new\", 1], {\"ok\": true}]",
                Some("json")
            )
            .unwrap(),
            vec!["[\"new\",1]", "{\"ok\":true}"],
            "the last complete array wins and nested arrays stay intact"
        );
        assert_eq!(
            split_items("[\"literal ] and [ text\", \"z\"]", Some("json")).unwrap(),
            vec!["literal ] and [ text", "z"],
            "brackets inside JSON strings do not end a candidate"
        );
        assert_eq!(split_items("[1, 2]", Some("json")).unwrap(), vec!["1", "2"]);
        assert_eq!(
            split_items("a---b---c", Some("separator:---")).unwrap(),
            vec!["a", "b", "c"]
        );
        assert!(split_items("nope", Some("json")).is_err());
        assert!(split_items("x", Some("separator:")).is_err());
        assert!(split_items("", None).unwrap().is_empty());
    }

    #[test]
    fn head_tail_keeps_both_ends() {
        let s = "START".to_string() + &"x".repeat(100) + "VERDICT: OK";
        let cut = head_tail(&s, 40);
        assert!(cut.starts_with("START"), "{cut}");
        assert!(cut.ends_with("VERDICT: OK"), "{cut}");
        assert!(cut.contains("truncated middle"));
        assert_eq!(head_tail("short", 100), "short");
    }

    #[test]
    fn foreach_input_fingerprint_binds_values_order_and_boundaries() {
        let original = vec!["ab".to_string(), "c".to_string()];
        assert_eq!(
            foreach_items_hash(&original),
            foreach_items_hash(&original.clone())
        );
        assert_ne!(
            foreach_items_hash(&original),
            foreach_items_hash(&["c".into(), "ab".into()])
        );
        assert_ne!(
            foreach_items_hash(&original),
            foreach_items_hash(&["a".into(), "bc".into()])
        );
    }

    #[test]
    fn failed_output_records_process_facts_without_changing_the_text() {
        assert_eq!(
            failed_output("verify[1]", "partial text", 9, false),
            "[sfh: step 'verify[1]' did not complete (exit=9, timed_out=false).\n \
             The text below is whatever it produced before failing. It is not a result.]\n\
             partial text"
        );
    }

    #[test]
    fn fingerprint_detects_flow_edits() {
        assert_eq!(fingerprint("a"), fingerprint("a"));
        assert_ne!(fingerprint("a"), fingerprint("b"));
        assert_eq!(fingerprint("").len(), 64);
        // The resume guard trusts this value; pin it to the known SHA-256 of "".
        assert_eq!(
            fingerprint(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        // The SAME flow checked out with CRLF and with LF is the same flow.
        // Hashing raw bytes made those two different versions of it, so a run
        // dir could not travel between a Windows and a Unix working copy - and
        // sfh's whole premise is that a flow file behaves identically on all
        // three. Every other cross-OS decision here is byte-identical.
        assert_eq!(
            fingerprint("name: x\nsteps:\n  - id: a\n"),
            fingerprint("name: x\r\nsteps:\r\n  - id: a\r\n")
        );
        // Still a change-detector: a lone CR that is not a line ending, and a
        // real edit, both move the hash.
        assert_ne!(fingerprint("a\rb"), fingerprint("ab"));
        assert_ne!(
            fingerprint("name: x\nsteps:\n"),
            fingerprint("name: y\nsteps:\n")
        );
        // Runs written by 0.9 recorded the RAW hash under a different algo id,
        // and must keep verifying the way 0.9 computed them.
        assert_ne!(FINGERPRINT_ALGO, RAW_SHA_FINGERPRINT_ALGO);
        assert_eq!(
            crate::sha256::hex("a\r\nb".as_bytes()),
            crate::sha256::hex("a\r\nb".as_bytes())
        );
        assert_ne!(
            fingerprint("a\r\nb"),
            crate::sha256::hex("a\r\nb".as_bytes())
        );
    }

    #[test]
    fn utc_stamps_round_trip_and_reject_junk() {
        // `sfh status` subtracts a stamp another process wrote from now, so the
        // parse has to be the exact inverse of the writer - a silently wrong
        // epoch would print a confident, invented "38m since last output".
        for secs in [0u64, 1_000_000_000, 1_769_000_000, 4_102_444_800] {
            assert_eq!(parse_utc_stamp(&utc_stamp_at(secs)), Some(secs));
        }
        for bad in [
            "",
            "20260729",
            "20260729-1749",
            "20260729-17490",
            "20260729_174900",
            "2026072a-174900",
            "20261329-174900",
            "20260729-254900",
        ] {
            assert_eq!(parse_utc_stamp(bad), None, "{bad} must not parse");
        }
    }

    #[test]
    fn legacy_fingerprint_is_fnv1a_16hex() {
        let fp = legacy_fingerprint_fnv("hello");
        assert_eq!(fp.len(), 16);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(fp, legacy_fingerprint_fnv("hello"));
        assert_ne!(fp, legacy_fingerprint_fnv("world"));
    }

    #[test]
    fn check_flow_fingerprint_honours_recorded_algo() {
        let flow_text = "name: test\nsteps:\n  - id: a\n    cmd: [\"echo\"]\n";
        let sha_fp = fingerprint(flow_text);
        let fnv_fp = legacy_fingerprint_fnv(flow_text);
        let dir = Path::new("/tmp/fake");
        let flow_path = Path::new("test.yaml");

        let meta_sha = serde_json::json!({
            "flow_fingerprint": sha_fp,
            "flow_fingerprint_algo": "sha256"
        });
        assert!(check_flow_fingerprint(&meta_sha, flow_text, dir, flow_path).is_ok());

        let meta_fnv = serde_json::json!({
            "flow_fingerprint": fnv_fp,
            "flow_fingerprint_algo": "fnv1a"
        });
        assert!(check_flow_fingerprint(&meta_fnv, flow_text, dir, flow_path).is_ok());

        let meta_legacy = serde_json::json!({
            "flow_fingerprint": fnv_fp
        });
        assert!(check_flow_fingerprint(&meta_legacy, flow_text, dir, flow_path).is_ok());

        let meta_wrong = serde_json::json!({
            "flow_fingerprint": sha_fp,
            "flow_fingerprint_algo": "fnv1a"
        });
        assert!(check_flow_fingerprint(&meta_wrong, flow_text, dir, flow_path).is_err());

        let meta_unknown = serde_json::json!({
            "flow_fingerprint": "abc",
            "flow_fingerprint_algo": "md5"
        });
        assert!(check_flow_fingerprint(&meta_unknown, flow_text, dir, flow_path).is_err());
    }

    #[test]
    fn effective_config_fingerprint_detects_merged_profile_changes() {
        let config = r#"{"api_version":1,"profiles":{"review":{"tool":"claude"}}}"#;
        let meta = serde_json::json!({
            "effective_config_fingerprint": fingerprint(config),
            "effective_config_fingerprint_algo": FINGERPRINT_ALGO
        });
        assert!(check_effective_config_fingerprint(&meta, config, Path::new("/tmp/run")).is_ok());
        let changed = r#"{"api_version":1,"profiles":{"review":{"tool":"claude","model":"new"}}}"#;
        let error =
            check_effective_config_fingerprint(&meta, changed, Path::new("/tmp/run")).unwrap_err();
        assert!(
            error.contains("different effective configuration"),
            "{error}"
        );
    }

    #[test]
    fn durable_log_events_carry_a_public_schema_version() {
        let path = std::env::temp_dir().join(format!(
            "sfh-log-schema-{}-{}.jsonl",
            std::process::id(),
            utc_stamp()
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        log_event(
            &mut file,
            json!({"ts": utc_stamp(), "event": "run_end", "status": "ok"}),
        )
        .unwrap();
        drop(file);
        let value: serde_json::Value =
            serde_json::from_str(std::fs::read_to_string(&path).unwrap().trim()).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(value["schema_version"], 1);
    }

    #[test]
    fn utc_stamp_has_stable_shape() {
        let s = utc_stamp();
        assert_eq!(s.len(), 15);
        assert_eq!(&s[8..9], "-");
        assert!(s.chars().filter(|c| c.is_ascii_digit()).count() == 14);
    }

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn one_line_marks_truncation() {
        assert_eq!(one_line("abc", 10), "abc");
        assert_eq!(one_line("abcdef", 3), "abc...");
        assert_eq!(one_line("a\nb", 10), "a...");
    }

    #[test]
    fn effort_vocab_warns_only_for_unknown_levels() {
        assert!(effort_vocab_warning("codex", "xhigh").is_none());
        assert!(effort_vocab_warning("grok", "max").is_some());
        assert!(effort_vocab_warning("opencode", "whatever").is_none());
        assert!(effort_vocab_warning("pi", "off").is_none());
        assert!(effort_vocab_warning("pi", "ultra").is_some());
    }

    // ---- P1-02: --carry-budget-from must PROVE the source is done, not
    // just fail to prove it is still going ----

    fn carry_final_test_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sfh-carry-final-{tag}-{}", contain::random_nonce()));
        contain::mkdir_private(&dir).unwrap();
        dir
    }

    #[test]
    fn carry_refuses_a_source_with_no_status_json_and_no_terminal_event_in_its_log() {
        let dir = carry_final_test_dir("no-proof");
        // A log that looks exactly like a run still in progress: a step
        // started, and nothing durable says it ever stopped.
        std::fs::write(
            dir.join("log.jsonl"),
            "{\"event\":\"step_start\",\"step\":\"a\",\"visit\":1}\n",
        )
        .unwrap();
        let err = carry_source_is_final(&dir).unwrap_err();
        assert!(
            err.contains("cannot confirm"),
            "expected a refusal explaining the proof is missing, got: {err}"
        );
        assert!(
            err.contains("sfh wait") && err.contains("sfh stop"),
            "the refusal must say how to resolve it, same as the still-going case: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn carry_is_allowed_when_the_log_holds_a_durable_run_end_event_even_with_no_status_json() {
        let dir = carry_final_test_dir("run-end-proof");
        std::fs::write(
            dir.join("log.jsonl"),
            "{\"event\":\"run_end\",\"status\":\"failed\",\"leaf_runs\":1,\"cost_usd\":0.5,\"elapsed_sec\":10}\n",
        )
        .unwrap();
        assert!(carry_source_is_final(&dir).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn carry_is_allowed_when_the_owning_process_is_confirmed_gone_and_the_log_lands_on_a_terminal_position(
    ) {
        let dir = carry_final_test_dir("dead-owner-proof");
        std::fs::write(
            dir.join("log.jsonl"),
            "{\"event\":\"position\",\"after\":\"a\",\"next\":\"fail\",\"via\":\"rule\"}\n",
        )
        .unwrap();
        // A live pid whose recorded start time does not match is a reused
        // pid, not the original owner - confirmed gone, the same reasoning
        // watch::owner_verifiably_dead's own tests exercise directly.
        contain::write_nonce(&dir, std::process::id(), Some(1), "tok").unwrap();
        assert!(carry_source_is_final(&dir).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn carry_still_refuses_a_terminal_looking_log_when_the_owning_process_cannot_be_confirmed_gone()
    {
        let dir = carry_final_test_dir("ambiguous-owner");
        std::fs::write(
            dir.join("log.jsonl"),
            "{\"event\":\"position\",\"after\":\"a\",\"next\":\"stuck\",\"via\":\"rule\"}\n",
        )
        .unwrap();
        // No sfh-nonce at all: nothing here rules out the owning process
        // still being alive and about to log run_end a moment later.
        let err = carry_source_is_final(&dir).unwrap_err();
        assert!(err.contains("cannot confirm"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn carry_refuses_a_source_that_status_json_says_is_still_running_even_if_its_log_has_a_run_end()
    {
        let dir = carry_final_test_dir("still-running");
        // Deliberately contradictory: status.json must win when it can be
        // read, because it is the freshest signal there is - a stale-but-
        // still-present run_end from an earlier attempt must not override a
        // status.json that says this run is going right now.
        std::fs::write(
            dir.join("log.jsonl"),
            "{\"event\":\"run_end\",\"status\":\"ok\",\"leaf_runs\":1,\"cost_usd\":1.0,\"elapsed_sec\":5}\n",
        )
        .unwrap();
        let status =
            serde_json::json!({"state": "running", "pid": std::process::id(), "cost_usd": 1.0});
        contain::write_private_atomic(&dir.join("status.json"), status.to_string()).unwrap();
        let err = carry_source_is_final(&dir).unwrap_err();
        assert!(err.contains("still going"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn carry_is_allowed_when_status_json_clearly_reads_a_terminal_state() {
        let dir = carry_final_test_dir("clean-status");
        let status = serde_json::json!({"state": "failed", "pid": 0});
        contain::write_private_atomic(&dir.join("status.json"), status.to_string()).unwrap();
        assert!(carry_source_is_final(&dir).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_terminal_evidence_only_trusts_the_logs_last_position_not_an_earlier_one() {
        let dir = carry_final_test_dir("last-position-wins");
        // A first attempt got stuck; a resumed second attempt got past it
        // and is now mid-step, with no terminal marker of its own yet. The
        // earlier stuck landing must not leak through as proof.
        std::fs::write(
            dir.join("log.jsonl"),
            "{\"event\":\"position\",\"after\":\"a\",\"next\":\"stuck\",\"via\":\"rule\"}\n\
             {\"event\":\"position\",\"after\":\"a\",\"next\":\"b\",\"via\":\"rule\"}\n",
        )
        .unwrap();
        let (run_end, terminal) = log_terminal_evidence(&dir).unwrap();
        assert!(!run_end);
        assert!(
            !terminal,
            "a later non-terminal position must supersede the earlier stuck landing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- P1-03: active time carried across --carry-budget-from must be
    // durable, not dependent on status.json surviving ----

    #[test]
    fn elapsed_restore_prefers_run_end_then_meta_then_status_then_the_carried_floor() {
        let base = std::env::temp_dir().join(format!(
            "sfh-elapsed-precedence-{}",
            contain::random_nonce()
        ));
        contain::mkdir_private(&base).unwrap();

        // status.json only: the last-resort source.
        let status_only = base.join("status-only");
        contain::mkdir_private(&status_only).unwrap();
        std::fs::write(status_only.join("log.jsonl"), "{\"event\":\"run_start\"}\n").unwrap();
        contain::write_private_atomic(
            &status_only.join("status.json"),
            serde_json::json!({"elapsed_sec": 10}).to_string(),
        )
        .unwrap();
        assert_eq!(load_resume(&status_only).unwrap().elapsed_sec, 10);

        // meta.json beats a smaller status.json.
        let meta_over_status = base.join("meta-over-status");
        contain::mkdir_private(&meta_over_status).unwrap();
        std::fs::write(
            meta_over_status.join("log.jsonl"),
            "{\"event\":\"run_start\"}\n",
        )
        .unwrap();
        contain::write_private_atomic(
            &meta_over_status.join("status.json"),
            serde_json::json!({"elapsed_sec": 10}).to_string(),
        )
        .unwrap();
        contain::write_private_atomic(
            &meta_over_status.join("meta.json"),
            serde_json::json!({"elapsed_sec": 20}).to_string(),
        )
        .unwrap();
        assert_eq!(load_resume(&meta_over_status).unwrap().elapsed_sec, 20);

        // A durable run_end beats both meta.json and status.json.
        let run_end_over_both = base.join("run-end-over-both");
        contain::mkdir_private(&run_end_over_both).unwrap();
        std::fs::write(
            run_end_over_both.join("log.jsonl"),
            "{\"event\":\"run_end\",\"status\":\"ok\",\"elapsed_sec\":30}\n",
        )
        .unwrap();
        contain::write_private_atomic(
            &run_end_over_both.join("status.json"),
            serde_json::json!({"elapsed_sec": 10}).to_string(),
        )
        .unwrap();
        contain::write_private_atomic(
            &run_end_over_both.join("meta.json"),
            serde_json::json!({"elapsed_sec": 20}).to_string(),
        )
        .unwrap();
        assert_eq!(load_resume(&run_end_over_both).unwrap().elapsed_sec, 30);

        // With nothing durable at all, the carried floor still holds.
        let floor_only = base.join("floor-only");
        contain::mkdir_private(&floor_only).unwrap();
        std::fs::write(
            floor_only.join("log.jsonl"),
            "{\"event\":\"budget_carried\",\"elapsed_sec\":40}\n",
        )
        .unwrap();
        assert_eq!(load_resume(&floor_only).unwrap().elapsed_sec, 40);

        // The floor wins even over a SMALLER durable value: this is a
        // synthetic, adversarial log (a real run_end can never report less
        // than what was durably carried in), built only to prove the floor
        // is applied unconditionally, the way a hand-edited log is tested
        // elsewhere in this file.
        let floor_over_run_end = base.join("floor-over-run-end");
        contain::mkdir_private(&floor_over_run_end).unwrap();
        std::fs::write(
            floor_over_run_end.join("log.jsonl"),
            "{\"event\":\"budget_carried\",\"elapsed_sec\":40}\n{\"event\":\"run_end\",\"status\":\"ok\",\"elapsed_sec\":5}\n",
        )
        .unwrap();
        assert_eq!(load_resume(&floor_over_run_end).unwrap().elapsed_sec, 40);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn carried_active_time_survives_two_hops_of_carry_even_when_every_status_json_is_gone() {
        // Simulates A -> B (--carry-budget-from A) -> C (--carry-budget-from
        // B), with status.json absent at EVERY hop: not merely stale, gone
        // outright, as if each run's status.json had already been cleaned up
        // by the time the next carry looked for it. Only the run_end events -
        // and the budget_carried events a real carry closure would write from
        // what it read back - survive. Before P1-03 this chain lost B's own
        // 50s the moment C tried to carry from B, because the only place B's
        // total lived was status.json.
        let base =
            std::env::temp_dir().join(format!("sfh-elapsed-chain-{}", contain::random_nonce()));
        contain::mkdir_private(&base).unwrap();

        let a = base.join("a");
        contain::mkdir_private(&a).unwrap();
        std::fs::write(
            a.join("log.jsonl"),
            "{\"event\":\"run_end\",\"status\":\"failed\",\"leaf_runs\":1,\"cost_usd\":1.0,\"elapsed_sec\":100}\n",
        )
        .unwrap();
        let a_elapsed = load_resume(&a).unwrap().elapsed_sec;
        assert_eq!(
            a_elapsed, 100,
            "A's own total must survive with no status.json at all"
        );

        let b = base.join("b");
        contain::mkdir_private(&b).unwrap();
        std::fs::write(
            b.join("log.jsonl"),
            format!(
                "{{\"event\":\"budget_carried\",\"elapsed_sec\":{a_elapsed}}}\n\
                 {{\"event\":\"run_end\",\"status\":\"failed\",\"leaf_runs\":1,\"cost_usd\":1.0,\"elapsed_sec\":150}}\n"
            ),
        )
        .unwrap();
        let b_elapsed = load_resume(&b).unwrap().elapsed_sec;
        assert_eq!(
            b_elapsed, 150,
            "B must recover A's 100s PLUS its own 50s, from run_end alone"
        );

        let c = base.join("c");
        contain::mkdir_private(&c).unwrap();
        std::fs::write(
            c.join("log.jsonl"),
            format!(
                "{{\"event\":\"budget_carried\",\"elapsed_sec\":{b_elapsed}}}\n\
                 {{\"event\":\"run_end\",\"status\":\"ok\",\"leaf_runs\":1,\"cost_usd\":1.0,\"elapsed_sec\":175}}\n"
            ),
        )
        .unwrap();
        let c_elapsed = load_resume(&c).unwrap().elapsed_sec;
        assert_eq!(
            c_elapsed, 175,
            "C must recover the full chain's 175s - B's carried total plus C's own 25s"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // ---- P1-09: next_actions_for must diagnose resume/carry, not assume
    // them from the terminal state alone ----

    fn next_actions_test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sfh-next-actions-{tag}-{}",
            contain::random_nonce()
        ));
        contain::mkdir_private(&dir).unwrap();
        dir
    }

    #[test]
    fn stuck_step_exhausted_max_visits_names_the_step_only_when_the_last_position_says_so() {
        let dir = next_actions_test_dir("max-visits-detect");
        assert_eq!(
            stuck_step_exhausted_max_visits(&dir),
            None,
            "no log at all is not evidence of anything"
        );
        std::fs::write(
            dir.join("log.jsonl"),
            "{\"event\":\"position\",\"after\":\"loopy\",\"next\":\"stuck\",\"via\":\"rule\"}\n",
        )
        .unwrap();
        assert_eq!(
            stuck_step_exhausted_max_visits(&dir),
            None,
            "an explicit goto: stuck rule is not a max_visits dead end"
        );
        std::fs::write(
            dir.join("log.jsonl"),
            "{\"event\":\"position\",\"after\":\"loopy\",\"next\":\"stuck\",\"via\":\"max_visits\"}\n",
        )
        .unwrap();
        assert_eq!(
            stuck_step_exhausted_max_visits(&dir),
            Some("loopy".to_string())
        );
        // A later, non-max_visits position (a subsequent resumed attempt
        // that got past it) must clear the earlier landing.
        std::fs::write(
            dir.join("log.jsonl"),
            "{\"event\":\"position\",\"after\":\"loopy\",\"next\":\"stuck\",\"via\":\"max_visits\"}\n\
             {\"event\":\"position\",\"after\":\"loopy\",\"next\":\"next_step\",\"via\":\"rule\"}\n",
        )
        .unwrap();
        assert_eq!(stuck_step_exhausted_max_visits(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_actions_for_a_done_run_offers_only_why() {
        let dir = PathBuf::from("/does-not-need-to-exist");
        let flow = PathBuf::from("flow.yaml");
        let actions = next_actions_for("done", &dir, &flow, None);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["kind"], "why");
    }

    #[test]
    fn next_actions_for_a_persistence_failure_does_not_offer_resume_but_still_offers_carry() {
        let dir = next_actions_test_dir("persistence-failure");
        // A healthy terminal status.json, so the carry diagnosis (a
        // separate question from resumability) resolves cleanly and this
        // test isolates the resume diagnosis.
        contain::write_private_atomic(
            &dir.join("status.json"),
            serde_json::json!({"state": "failed", "pid": 0}).to_string(),
        )
        .unwrap();
        let flow = PathBuf::from("flow.yaml");
        let error = Some((
            machine::ErrorCode::PersistenceFailure,
            "step 'x': required result artifacts could not be persisted",
        ));
        let actions = next_actions_for("failed", &dir, &flow, error);

        let resume = actions
            .iter()
            .find(|a| a["kind"] == "resume")
            .expect("a resume diagnosis is always present, even when refused");
        assert_eq!(resume["resumable"], false);
        assert!(
            resume.get("argv").is_none(),
            "an action that cannot succeed must carry no argv to run: {resume}"
        );
        assert!(resume["reason"].as_str().unwrap().contains("persist"));

        let carry = actions
            .iter()
            .find(|a| a["kind"] == "carry_budget")
            .expect("a carry diagnosis is always present");
        assert_eq!(carry["carryable"], true);
        assert!(carry.get("argv").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_actions_for_a_stuck_run_that_exhausted_max_visits_does_not_offer_resume() {
        let dir = next_actions_test_dir("stuck-max-visits");
        std::fs::write(
            dir.join("log.jsonl"),
            "{\"event\":\"position\",\"after\":\"loopy\",\"next\":\"stuck\",\"via\":\"max_visits\"}\n",
        )
        .unwrap();
        contain::write_private_atomic(
            &dir.join("status.json"),
            serde_json::json!({"state": "stuck", "pid": 0}).to_string(),
        )
        .unwrap();
        let flow = PathBuf::from("flow.yaml");
        let actions = next_actions_for("stuck", &dir, &flow, None);
        let resume = actions.iter().find(|a| a["kind"] == "resume").unwrap();
        assert_eq!(resume["resumable"], false);
        assert!(resume["reason"].as_str().unwrap().contains("max_visits"));
        assert!(resume.get("argv").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_actions_for_an_ordinary_stuck_landing_still_offers_resume() {
        let dir = next_actions_test_dir("stuck-ordinary");
        std::fs::write(
            dir.join("log.jsonl"),
            "{\"event\":\"position\",\"after\":\"review\",\"next\":\"stuck\",\"via\":\"rule\"}\n",
        )
        .unwrap();
        contain::write_private_atomic(
            &dir.join("status.json"),
            serde_json::json!({"state": "stuck", "pid": 0}).to_string(),
        )
        .unwrap();
        let flow = PathBuf::from("flow.yaml");
        let actions = next_actions_for("stuck", &dir, &flow, None);
        let resume = actions.iter().find(|a| a["kind"] == "resume").unwrap();
        assert_eq!(resume["resumable"], true);
        let argv: Vec<String> = resume["argv"]
            .as_array()
            .expect("a runnable action carries argv")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(argv.contains(&"--resume".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_actions_for_a_workspace_drift_failure_offers_resume_with_adopt_workspace_baked_in() {
        let dir = next_actions_test_dir("workspace-drift");
        contain::write_private_atomic(
            &dir.join("status.json"),
            serde_json::json!({"state": "failed", "pid": 0}).to_string(),
        )
        .unwrap();
        let flow = PathBuf::from("flow.yaml");
        let error = Some((
            machine::ErrorCode::WorkspaceDrift,
            "SFH_WORKSPACE_DRIFT: the managed workspace changed",
        ));
        let actions = next_actions_for("failed", &dir, &flow, error);
        let resume = actions.iter().find(|a| a["kind"] == "resume").unwrap();
        assert_eq!(resume["resumable"], true);
        assert_eq!(resume["requires"], serde_json::json!(["--adopt-workspace"]));
        let argv: Vec<String> = resume["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(
            argv.contains(&"--adopt-workspace".to_string()),
            "a caller executing this argv verbatim must not be missing the flag the diagnosis named: {argv:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
