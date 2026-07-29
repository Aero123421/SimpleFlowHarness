use crate::{contain, execute, flow, leaf, preset, template};
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
}

pub fn run(opts: RunOpts) -> i32 {
    match run_inner(&opts) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sfh: {e}");
            2
        }
    }
}

pub fn validate(path: &Path, var_overrides: &[(String, String)]) -> i32 {
    let inner = || -> Result<(), String> {
        let flow = flow::load(path)?;
        let mut vars = flow.vars_string_map()?;
        for (k, v) in var_overrides {
            vars.insert(k.clone(), v.clone());
        }
        precheck(&flow, &vars, &HashSet::new())?;
        eprintln!("OK: {} ({} steps)", path.display(), flow.steps.len());
        for s in &flow.steps {
            eprintln!("  - {} ({})", s.id, describe_kind(&flow, s));
            if let Some(children) = &s.parallel {
                for c in children {
                    eprintln!("      * {} ({})", c.id, describe_kind(&flow, c));
                }
            }
        }
        Ok(())
    };
    match inner() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("sfh: {e}");
            2
        }
    }
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
            for t in [
                &r.when_contains,
                &r.when_matches,
                &r.when_last_line_contains,
                &r.when_last_line_is,
                &r.when_last_line_matches,
            ]
            .into_iter()
            .flatten()
            {
                chk(&ctx_base, "route condition", t)?;
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
    fn of(defaults: &flow::Defaults, flow_start: Instant) -> Option<Self> {
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
            wall_at: defaults
                .wall_clock_sec
                .map(|s| flow_start + Duration::from_secs(s.saturating_sub(reserve_sec))),
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

#[derive(Clone)]
struct PendingRoute {
    step: String,
    visit: u32,
    route_text: String,
    /// True when route_text came from a fan-out's headerless plain output.
    /// Compaction rewrites the chain file but never changes what live routing
    /// matched against, so a plain-sourced route must NOT be patched from the
    /// precompact file the way a leaf's chain-sourced route is.
    from_plain: bool,
}

#[derive(Clone)]
struct UnfinishedStep {
    step: String,
    started: String,
    cmd: String,
}

#[derive(Default)]
struct ResumeState {
    outputs: BTreeMap<String, template::StepOutput>,
    visits: HashMap<String, u32>,
    sessions: HashMap<String, leaf::SessionInfo>,
    chain_files: HashMap<String, PathBuf>,
    total: u32,
    cost_usd: f64,
    start: Option<String>,
    pending_route: Option<PendingRoute>,
    unfinished_step: Option<UnfinishedStep>,
    last_executed: Option<String>,
    last_success: Option<String>,
    completed: bool,
    /// True once this run has already spent its one `on_budget` landing. The
    /// log is the only record of it: without this the resumed run would arrive
    /// with the restored cost still over the threshold and land a second time,
    /// which turns "one wrap-up chain per run" into "one per crash".
    budget_landed: bool,
    /// Fan-out members that already finished in a crashed attempt, keyed by
    /// (parent step id, visit). A resume that re-runs a parallel/foreach group
    /// SKIPS these instead of executing them a second time: re-running spent
    /// money twice, opened duplicate sessions, and could push the restored
    /// count plus a full fresh batch past max_total_steps, wedging the resume
    /// (rev_regression: fan-out members re-executed after a mid-group crash).
    completed_members: HashMap<(String, u32), HashSet<String>>,
}

fn load_resume(run_dir: &Path) -> Result<ResumeState, String> {
    // Contained, no-follow read: log.jsonl is a fixed name in a directory an
    // attacker controls on --resume, and a symlink there used to be followed
    // to an external JSONL file that was then ingested as the run's entire
    // restored state (rev_break #6). A missing log is a hard error (the caller
    // already verified the dir looks like a run).
    let log = contain::read_contained_opt(run_dir, "log.jsonl")?
        .ok_or_else(|| format!("cannot read {}/log.jsonl: file missing", run_dir.display()))?;
    let mut st = ResumeState::default();
    let mut last_step: Option<String> = None;
    let mut unfinished: BTreeMap<(String, u32), UnfinishedStep> = BTreeMap::new();
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
            // A compacted step's chain file now holds the summary, so `output`
            // is already right. `outputs` has to be walked back to what was
            // summarized, or a resumed run sees strictly less than a live one.
            "compact_end" | "compact_failed" => {
                // A live run counts the summarizer as one more leaf run and
                // adds its cost the moment compact starts; a resume that drops
                // both under-reports steps_done and cost, which can push a
                // resumed run past max_total_steps / max_cost_usd that the
                // live run would have honoured.
                st.total += 1;
                if ev == "compact_end" {
                    if let Some(c) = v.get("cost_usd").and_then(|x| x.as_f64()) {
                        st.cost_usd += c.max(0.0);
                    }
                }
                // A path that escapes the run dir fails the resume outright;
                // only a genuinely absent file reads as None (S1-4).
                let precompact = match v.get("precompact_file").and_then(|x| x.as_str()) {
                    Some(p) => contain::read_contained_opt(run_dir, p)?,
                    None => None,
                };
                if let (Some(e), Some(p)) = (st.outputs.get_mut(&step), precompact.as_ref()) {
                    e.outputs = p.trim_end().to_string();
                }
                if let (Some(pending), Some(p)) = (st.pending_route.as_mut(), precompact) {
                    if pending.step == step && !pending.from_plain {
                        // Live routing uses the pre-compact text, even though
                        // the chain file now contains the summary/head+tail.
                        // A fan-out's route text is its headerless plain
                        // output, which compaction never touches.
                        pending.route_text = p.trim_end().to_string();
                    }
                }
            }
            "step_start" => {
                let visit = v.get("visit").and_then(|x| x.as_u64()).unwrap_or(1) as u32;
                unfinished.insert(
                    (step.clone(), visit),
                    UnfinishedStep {
                        step,
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
                    },
                );
            }
            // A fan-out logs NO step_start (its members do), so without these
            // events a crash mid-fan-out - before aggregate_end - leaves no
            // record of where the run was. A flow whose FIRST step is a fan-out
            // then has nothing to resume from at all. Track the group exactly
            // like an unfinished leaf; aggregate_end clears it.
            "group_start" | "foreach_start" => {
                let visit = v.get("visit").and_then(|x| x.as_u64()).unwrap_or(1) as u32;
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
                        started: v
                            .get("ts")
                            .and_then(|x| x.as_str())
                            .unwrap_or("unknown")
                            .to_string(),
                        cmd: format!("{kind} fan-out ({n} members)"),
                        step,
                    },
                );
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
                    let visit = v.get("visit").and_then(|x| x.as_u64()).unwrap_or(1) as u32;
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
                    st.total += 1;
                }
                // A run dir is untrusted on --resume: a NEGATIVE cost_usd in an
                // edited log would subtract from the running total and let a
                // resumed run slip under max_cost_usd. Reported cost can only be
                // spent, never refunded, so clamp at zero (rev_break #12).
                if let Some(c) = v.get("cost_usd").and_then(|x| x.as_f64()) {
                    st.cost_usd += c.max(0.0);
                }
                let visit = v.get("visit").and_then(|x| x.as_u64()).unwrap_or(1) as u32;
                let is_child = v.get("parent").is_some_and(|p| !p.is_null());
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
                    Some(p) => contain::read_contained_opt(run_dir, p)?.unwrap_or_default(),
                    None => String::new(),
                };
                // Fan-out steps route against the headerless plain
                // concatenation, NOT the labeled aggregate the chain file
                // holds: live routing matches `plain`, so a resume that
                // re-reads the chain would test conditions against "--- id ---"
                // headers and could pick a different branch.
                let plain = match v.get("plain_file").and_then(|x| x.as_str()) {
                    Some(p) => contain::read_contained_opt(run_dir, p)?,
                    None => None,
                };
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
                    st.pending_route = if ev == "step_end" && ok {
                        Some(PendingRoute {
                            step: step.clone(),
                            visit,
                            route_text: chain.trim_end().to_string(),
                            from_plain: false,
                        })
                    } else if ev == "aggregate_end" && ok {
                        // Runs written before plain_file existed have no
                        // headerless copy: leave the route unset (as before)
                        // so the fan-out re-runs rather than routing on
                        // headered text live never matched against.
                        plain.map(|p| PendingRoute {
                            step: step.clone(),
                            visit,
                            route_text: p.trim_end().to_string(),
                            from_plain: true,
                        })
                    } else {
                        None
                    };
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
            "budget_landing" => st.budget_landed = true,
            "run_end" => {}
            _ => {}
        }
    }
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
                // extend, not or_insert: a member known complete from either
                // source is complete, and replacing an existing entry - or
                // declining to touch it - would drop one side of the union.
                st.completed_members
                    .entry((resume_at, visit + 1))
                    .or_default()
                    .extend(set);
            }
        }
    }
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
        .filter(|p| p.join("log.jsonl").exists())
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
    if opts.no_partial_emit {
        args.push("--no-partial-emit".into());
    }
    if opts.verbose {
        args.push("--verbose".into());
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
    let _ = std::fs::remove_file(&status_path);

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
    contain::write_nonce(run_dir, d.pid, child_start, nonce)
        .map_err(|e| format!("cannot write the stop nonce: {e}"))?;
    // Seed status.json only if the child has not already written its own, so
    // `sfh status` has something to report either way and neither clobbers.
    // The create_new guard inside seed_status makes this race-free.
    seed_status(
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
            run_dir: run_dir.display().to_string(),
            flow: abs(&opts.flow_path).display().to_string(),
            pid: d.pid,
            exit_code: None,
            emit_step: None,
            emit_file: None,
            error: None,
            unfinished_step: None,
            nonce: nonce.to_string(),
            pid_start: child_start,
        },
    );
    if !opts.quiet {
        eprintln!(
            "sfh: detached (pid {}). poll with: sfh status {}",
            d.pid,
            execute::shell_quote(&run_dir.display().to_string())
        );
    }
    // stdout gets the run dir and nothing else, so a caller can capture it.
    println!("{}", run_dir.display());
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
    unfinished_step: Option<UnfinishedStep>,
    /// Random token proving this status.json was written by the sfh that owns
    /// the run dir. `sfh stop` refuses to kill without a matching nonce file.
    nonce: String,
    /// Start time of the owning process, so a reused pid is told apart from
    /// the process that started the run (rev_break #8).
    pid_start: Option<u64>,
}

fn status_json(s: &Status) -> String {
    let unfinished_step = s.unfinished_step.as_ref().map(|u| {
        json!({
            "step": u.step,
            "started_utc": u.started,
            "cmd": u.cmd,
            "will_rerun": true,
        })
    });
    let last_output = match s.last_output.load(std::sync::atomic::Ordering::Relaxed) {
        0 => serde_json::Value::Null,
        secs => json!(utc_stamp_at(secs)),
    };
    let v = json!({
        "state": s.state,
        "current_step": s.step,
        "started_utc": s.started,
        "step_started_utc": s.step_started,
        "last_output_utc": last_output,
        "visit": s.visit,
        "heartbeat_utc": utc_stamp(),
        "steps_done": s.steps_done,
        "cost_usd": s.cost_usd,
        "run_dir": s.run_dir,
        "flow": s.flow,
        "pid": s.pid,
        "sfh_version": VERSION,
        "exit_code": s.exit_code,
        "emit_step": s.emit_step,
        "emit_file": s.emit_file,
        "error": s.error,
        "unfinished_step": unfinished_step,
        "nonce": s.nonce,
        "pid_start": s.pid_start,
    });
    serde_json::to_string_pretty(&v).unwrap_or_default()
}

fn write_status(path: &Path, s: &Status) {
    let text = status_json(s);
    // Write-then-rename: `sfh status` and `sfh wait` poll this file every few
    // seconds, and a plain write lets them read a half-written document. Rename
    // is atomic on both platforms, so a reader sees the old or the new file and
    // never a torn one. The tmp name carries the pid: the detaching parent and
    // its child both write status.json, and a SHARED tmp name let them clobber
    // each other's in-flight write (rev_regression: detach status race).
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(".tmp.{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    if contain::write_private(&tmp, &text).is_ok() && std::fs::rename(&tmp, path).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(&tmp);
    let _ = contain::write_private(path, &text);
}

/// Seed status.json for a detached run ONLY if the child has not already written
/// its own. `create_new` is the atomic guard: an `exists()` check followed by a
/// write is not exclusive, so a short flow's child could finish (writing `done`)
/// between the parent's check and its write, and the parent's `running` seed
/// would then clobber the terminal status, leaving a finished run stuck on
/// `running` forever (rev_regression: detach status race). create_new fails
/// harmlessly once the file exists.
fn seed_status(path: &Path, s: &Status) {
    use std::io::Write as _;
    let text = status_json(s);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        let _ = f.write_all(text.as_bytes());
        contain::restrict_file(&f);
    }
}

fn run_inner(opts: &RunOpts) -> Result<i32, String> {
    let runs_root = opts
        .runs_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(".sfh").join("runs"));

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
    let flow = match flow::load(&opts.flow_path) {
        Ok(f) => f,
        Err(e) => {
            if resume_dir.is_some() && legacy_era {
                flow::load_lenient(&opts.flow_path).map_err(|_| e)?
            } else {
                return Err(e);
            }
        }
    };
    let mut vars = flow.vars_string_map()?;
    let step_ids = flow.step_ids();
    if let Some(e) = &opts.emit {
        if !step_ids.contains(e) {
            return Err(format!("--emit '{e}' is not a step id"));
        }
    }

    let flow_dir = abs(opts.flow_path.parent().unwrap_or(Path::new(".")));
    let flow_text = std::fs::read_to_string(&opts.flow_path).unwrap_or_default();
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
    if opts.dry_run {
        protect_runs_root(&runs_root)?;
        let base = format!("{}-{}-dryrun", utc_stamp(), name);
        run_dir = abs(&runs_root.join(base));
        contain::mkdir_private(&run_dir)
            .map_err(|e| format!("cannot create run dir {}: {e}", run_dir.display()))?;
    } else if let Some(dir) = resume_dir {
        // The resumed dir was protected when it was first created; nothing
        // here writes to the runs root, so its state is not this run's
        // concern (and the root may be absent or read-only by design).
        resumed = load_resume(&dir)?;
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
            eprintln!(
                "sfh: step '{}' started {} and never recorded an end.\n     resuming will run it again: {}",
                u.step, u.started, u.cmd
            );
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
    } else {
        protect_runs_root(&runs_root)?;
        let base = format!("{}-{}", utc_stamp(), name);
        let mut d = runs_root.join(&base);
        let mut n = 1;
        while d.exists() {
            n += 1;
            d = runs_root.join(format!("{base}-{n}"));
        }
        contain::mkdir_private(&d)
            .map_err(|e| format!("cannot create run dir {}: {e}", d.display()))?;
        run_dir = abs(&d);
    }
    // Defense-in-depth: even though `name` is charset-validated, confirm the
    // resolved run dir is actually under the runs root (guards symlink tricks).
    if !is_resume && opts.run_dir.is_none() && !contain::is_under(&runs_root, &run_dir) {
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
    if !opts.detach {
        // The detaching parent writes the file itself, after the spawn, when
        // it knows the child's pid (in detach_run).
        contain::write_nonce(&run_dir, std::process::id(), pid_start, &nonce)
            .map_err(|e| format!("cannot write the stop nonce: {e}"))?;
    }
    let notes_file = run_dir.join("notes.md");

    if opts.dry_run {
        return dry_run(
            &flow,
            &vars,
            &tainted_vars,
            &run_dir,
            &flow_dir,
            &notes_file,
            &needed_sessions,
        );
    }

    // ---- hand the run off to a detached copy of ourselves ----
    // Everything above this point is validation, so a broken flow still fails
    // in the caller's face instead of dying silently in the background.
    if opts.detach {
        return detach_run(opts, &run_dir, is_resume, &nonce);
    }

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
    let started = utc_stamp();
    let meta = json!({
        "sfh_version": VERSION,
        "flow": abs(&opts.flow_path).display().to_string(),
        "flow_fingerprint": flow_fp,
        "flow_fingerprint_algo": FINGERPRINT_ALGO,
        "name": name,
        "started_utc": started,
        "os": std::env::consts::OS,
        "vars": vars,
        "tools": tool_versions,
        "resumed": is_resume,
    });
    let _ = contain::write_private(
        &run_dir.join("meta.json"),
        serde_json::to_string_pretty(&meta).unwrap_or_default(),
    );
    log_event(
        &mut log,
        json!({"ts": utc_stamp(), "event": "run_start", "sfh_version": VERSION, "resumed": is_resume, "flow_fingerprint": flow_fp}),
    );

    // ---- live status file + heartbeat so a parent agent can poll liveness ----
    // The run-level activity clock: every child's reader thread stores the
    // moment it read anything here, so the heartbeat can publish "nothing has
    // been said for N minutes" without asking any single step.
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
        run_dir: run_dir.display().to_string(),
        flow: abs(&opts.flow_path).display().to_string(),
        pid: std::process::id(),
        exit_code: None,
        emit_step: None,
        emit_file: None,
        error: None,
        unfinished_step: resumed.unfinished_step.clone(),
        nonce: nonce.clone(),
        pid_start,
    }));
    {
        let s = Arc::clone(&status);
        let p = status_path.clone();
        std::thread::spawn(move || loop {
            {
                let g = s.lock().unwrap();
                if g.state != "running" {
                    write_status(&p, &g);
                    break;
                }
                write_status(&p, &g);
            }
            std::thread::sleep(Duration::from_secs(3));
        });
    }

    let index_of: HashMap<String, usize> = flow
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone(), i))
        .collect();
    let mut outputs = resumed.outputs;
    let mut visits = resumed.visits;
    let mut sessions = resumed.sessions;
    let mut total: u32 = resumed.total;
    let mut cost_usd: f64 = resumed.cost_usd;
    let mut last_executed = resumed.last_executed;
    let mut last_success = resumed.last_success;
    let pending_route = resumed.pending_route;
    let completed_members = resumed.completed_members;
    // step id -> the chain file its LAST visit wrote. A re-visited step writes
    // <id>.v2.chain.txt, so nothing may assume <id>.chain.txt.
    let mut chain_files = resumed.chain_files;
    let mut cur = match &resumed.start {
        Some(id) => *index_of
            .get(id)
            .ok_or_else(|| format!("resume: step '{id}' no longer exists in the flow"))?,
        None => 0,
    };
    if is_resume && !opts.quiet {
        if let Some(p) = &pending_route {
            eprintln!(
                "sfh: resuming {} by re-evaluating routing after step '{}' ({} steps already done, ${cost_usd:.4} spent)",
                run_dir.display(),
                p.step,
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
    // One clock for both the ceiling and the landing threshold, so the two can
    // never disagree about how long this attempt has been running. A resume
    // starts it again from zero, exactly as wall_clock_sec always has.
    let flow_start = Instant::now();
    let wall_deadline = flow
        .defaults
        .wall_clock_sec
        .map(|s| flow_start + Duration::from_secs(s));
    let budget_plan = BudgetPlan::of(&flow.defaults, flow_start);
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
                // The restored spend is real; the clock is this attempt's, the
                // same one wall_clock_sec is judged on.
                budget: leaf::BudgetVars::new(
                    &flow.defaults,
                    cost_usd,
                    flow_start.elapsed().as_secs(),
                ),
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
            let target = evaluate_route(step, &pending.route_text, &ctx)?;
            match target.as_ref().map(|h| (h.goto.as_str(), h)) {
                None => {
                    log_position(
                        &mut log,
                        &step.id,
                        next_label(completed_idx + 1, &flow),
                        PositionVia::Fallthrough,
                        None,
                    );
                    cur = completed_idx + 1;
                    if cur >= n_steps {
                        return Ok(FlowEnd::Completed);
                    }
                }
                Some(("end", hit)) => {
                    log_position(&mut log, &step.id, "end".into(), hit.via, Some(hit));
                    return Ok(FlowEnd::Completed);
                }
                Some(("fail", hit)) => {
                    log_position(&mut log, &step.id, "fail".into(), hit.via, Some(hit));
                    return Err(format!("step '{}' routed to fail", step.id));
                }
                Some(("stuck", hit)) => {
                    log_position(&mut log, &step.id, "stuck".into(), hit.via, Some(hit));
                    return Ok(FlowEnd::Stuck {
                        after: step.id.clone(),
                    });
                }
                Some((id, hit)) => {
                    if !opts.quiet {
                        eprintln!("sfh: [{}] -> goto {id}", step.id);
                    }
                    log_position(&mut log, &step.id, id.to_string(), hit.via, Some(hit));
                    cur = index_of[id];
                }
            }
        }
        loop {
            if execute::interrupted() {
                return Err("interrupted (Ctrl+C): child processes were terminated".into());
            }
            // F5: land before the cliff, once per run. Checked BEFORE the two
            // ceiling checks below, so a flow that reserves nothing still lands
            // at the exact point the hard error used to fire instead of racing
            // it. After the landing this whole block is skipped and the ceiling
            // checks are all that is left - spend the reserve too and the run
            // ends the way it always did.
            if let (Some(plan), false) = (&budget_plan, budget_landed) {
                let elapsed = flow_start.elapsed();
                if let Some(trigger) = plan.trigger(Instant::now(), cost_usd) {
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
                               "spent_usd": cost_usd, "elapsed_sec": elapsed.as_secs(),
                               "goto": plan.goto}),
                    );
                    if !opts.quiet {
                        eprintln!(
                            "sfh: budget landing ({trigger}): ${cost_usd:.4} spent, {}s elapsed -> goto {}",
                            elapsed.as_secs(),
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
                            );
                            return Ok(FlowEnd::Completed);
                        }
                        "fail" => {
                            log_position(
                                &mut log,
                                &pending_step,
                                "fail".into(),
                                PositionVia::Budget,
                                None,
                            );
                            return Err(format!(
                                "on_budget ({trigger}) routed to fail: ${cost_usd:.4} spent, {}s elapsed",
                                elapsed.as_secs()
                            ));
                        }
                        "stuck" => {
                            log_position(
                                &mut log,
                                &pending_step,
                                "stuck".into(),
                                PositionVia::Budget,
                                None,
                            );
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
                            );
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
            {
                let mut g = status.lock().unwrap();
                g.step = step.id.clone();
                g.steps_done = total;
                g.cost_usd = cost_usd;
            }

            let visit = visits.get(&step.id).copied().unwrap_or(0) + 1;
            let max_v = step.max_visits.or(flow.defaults.max_visits).unwrap_or(5);
            if visit > max_v {
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
                );
                match action {
                    "continue" => {
                        log_position(
                            &mut log,
                            &step.id,
                            next_label(cur + 1, &flow),
                            PositionVia::MaxVisits,
                            None,
                        );
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
                            );
                            return Ok(FlowEnd::Completed);
                        }
                        "fail" => {
                            log_position(
                                &mut log,
                                &step.id,
                                "fail".into(),
                                PositionVia::MaxVisits,
                                None,
                            );
                            return Err(format!("step '{}' exhausted max_visits ({max_v})", step.id));
                        }
                        "stuck" => {
                            log_position(
                                &mut log,
                                &step.id,
                                "stuck".into(),
                                PositionVia::MaxVisits,
                                None,
                            );
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
                            );
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
                        // Read at expansion time, so every step (and every
                        // retry, fallback and compaction inside it) renders
                        // {{budget.*}} from the totals as they stand now.
                        budget: leaf::BudgetVars::new(
                            &flow.defaults,
                            cost_usd,
                            flow_start.elapsed().as_secs(),
                        ),
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
                ($done:expr, $mstep:expr, $label:expr, $mtag:expr, $fbs:expr, $extra:expr) => {{
                    if !$done.ok() && !$done.interrupted && !$fbs.is_empty() {
                        for fb in $fbs {
                            if execute::interrupted() {
                                break;
                            }
                            cost_usd += $done.usage.cost_usd.unwrap_or(0.0);
                            log_step_end(
                                &mut log,
                                &$label,
                                Some(&step.id),
                                visit,
                                &$done,
                            );
                            if !opts.quiet {
                                eprintln!("sfh: [{}] falling back to profile '{fb}'", $label);
                            }
                            let ftag = format!("{}.fb-{fb}", $mtag);
                            let prep = {
                                let cx = mk_cx!(&outputs, &sessions);
                                leaf::prepare_leaf(&cx, $mstep, visit, &ftag, $extra, Some(fb))?
                            };
                            log_event(
                                &mut log,
                                json!({"ts": utc_stamp(), "event": "fallback", "step": $label, "profile": fb, "cmd": prep.inv.describe()}),
                            );
                            total += 1;
                            let alt = leaf::exec_leaf(prep);
                            let ok = alt.ok();
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
            let (mut chain_output, route_text, errored): (String, String, bool) = if let Some(
                children,
            ) =
                &step.parallel
            {
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
                for (ci, c) in children.iter().enumerate() {
                    if restored.contains(&c.id) {
                        continue;
                    }
                    let ctag = if visit == 1 {
                        c.id.clone()
                    } else {
                        format!("{}.v{visit}", c.id)
                    };
                    preps.push(leaf::prepare_leaf(&cx, c, visit, &ctag, &[], None)?);
                    fresh_idx.push(ci);
                }
                if total + preps.len() as u32 > max_total {
                    return Err(format!(
                            "step '{}' would bring total leaf runs to {} over max_total_steps ({max_total})",
                            step.id,
                            total + preps.len() as u32
                        ));
                }
                total += preps.len() as u32;
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
                log_restored_members(&mut log, &step.id, visit, &restored);
                if !preps.is_empty() {
                    log_event(
                        &mut log,
                        json!({"ts": utc_stamp(), "event": "group_start", "step": step.id, "visit": visit, "children": preps.len()}),
                    );
                }
                let mut dones = leaf::run_pool(preps, mp, Arc::clone(&gate));
                for (pos, &ci) in fresh_idx.iter().enumerate() {
                    let c = &children[ci];
                    let ctag = if visit == 1 {
                        c.id.clone()
                    } else {
                        format!("{}.v{visit}", c.id)
                    };
                    fan_fallback!(dones[pos], c, c.id, ctag, &c.fallback, &[]);
                }
                let mut agg = String::new();
                let mut plain = String::new();
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
                    cost_usd += d.usage.cost_usd.unwrap_or(0.0);
                    outputs.insert(
                        c.id.clone(),
                        template::StepOutput {
                            output: exposed.clone(),
                            outputs: exposed.clone(),
                            output_file: d.out_file.display().to_string(),
                            exit: d.exit_code,
                            stderr_file: stderr_file_for(&d.out_file).display().to_string(),
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
                    log_step_end(&mut log, &c.id, Some(&step.id), visit, d);
                    let failed_header = if d.ok() {
                        String::new()
                    } else {
                        format!(
                            " [sfh: FAILED exit={}, timed_out={}]",
                            d.exit_code, d.timed_out
                        )
                    };
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
                let _ = contain::write_private(&run_dir.join(&plain_name), &plain);
                write_aggregate(&run_dir, &gtag, &agg, &mut outputs, &step.id, hard_fail);
                log_aggregate_end(
                    &mut log,
                    &step.id,
                    visit,
                    &gtag,
                    hard_fail,
                    &plain,
                    &plain_name,
                );
                (agg, plain, hard_fail)
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
                    preps.push(leaf::prepare_leaf(
                        &cx,
                        step,
                        visit,
                        &tag,
                        &[("item", it.clone()), ("item_index", i.to_string())],
                        None,
                    )?);
                    fresh_idx.push(i);
                }
                if total + preps.len() as u32 > max_total {
                    return Err(format!(
                            "step '{}' would bring total leaf runs to {} over max_total_steps ({max_total})",
                            step.id,
                            total + preps.len() as u32
                        ));
                }
                total += preps.len() as u32;
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
                log_restored_members(&mut log, &step.id, visit, &restored);
                if !preps.is_empty() {
                    log_event(
                        &mut log,
                        json!({"ts": utc_stamp(), "event": "foreach_start", "step": step.id, "visit": visit, "items": preps.len()}),
                    );
                }
                let mut dones = leaf::run_pool(preps, mp, Arc::clone(&gate));
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
                    fan_fallback!(dones[pos], step, label, tag, &step.fallback, &extra);
                }
                let mut agg = String::new();
                let mut plain = String::new();
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
                    cost_usd += d.usage.cost_usd.unwrap_or(0.0);
                    log_step_end(&mut log, &label, Some(&step.id), visit, d);
                    let failed_header = if d.ok() {
                        String::new()
                    } else {
                        format!(
                            " [sfh: FAILED exit={}, timed_out={}]",
                            d.exit_code, d.timed_out
                        )
                    };
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
                let _ = contain::write_private(&run_dir.join(&plain_name), &plain);
                write_aggregate(&run_dir, &gtag, &agg, &mut outputs, &step.id, hard_fail);
                log_aggregate_end(
                    &mut log,
                    &step.id,
                    visit,
                    &gtag,
                    hard_fail,
                    &plain,
                    &plain_name,
                );
                (agg, plain, hard_fail)
            } else {
                if total + 1 > max_total {
                    return Err(format!("exceeded max_total_steps ({max_total})"));
                }
                total += 1;
                let mut done = {
                    let cx = mk_cx!(&outputs, &sessions);
                    let prep = leaf::prepare_leaf(&cx, step, visit, &gtag, &[], None)?;
                    log_event(
                        &mut log,
                        json!({"ts": utc_stamp(), "event": "step_start", "step": step.id, "visit": visit, "cmd": prep.inv.describe(), "session_parent": session_parent_json(&prep)}),
                    );
                    leaf::exec_leaf(prep)
                };
                // fallback: retry the step with a different profile (tool/model).
                if !done.ok() && !done.interrupted && !step.fallback.is_empty() {
                    for fb in &step.fallback {
                        if execute::interrupted() {
                            break;
                        }
                        cost_usd += done.usage.cost_usd.unwrap_or(0.0);
                        log_step_end(&mut log, &step.id, None, visit, &done);
                        if !opts.quiet {
                            eprintln!("sfh: [{}] falling back to profile '{fb}'", step.id);
                        }
                        let ftag = format!("{gtag}.fb-{fb}");
                        let cx = mk_cx!(&outputs, &sessions);
                        let prep = leaf::prepare_leaf(&cx, step, visit, &ftag, &[], Some(fb))?;
                        log_event(
                            &mut log,
                            json!({"ts": utc_stamp(), "event": "fallback", "step": step.id, "profile": fb, "cmd": prep.inv.describe()}),
                        );
                        total += 1;
                        let alt = leaf::exec_leaf(prep);
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
                cost_usd += d.usage.cost_usd.unwrap_or(0.0);
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
                log_step_end(&mut log, &step.id, None, visit, d);
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
                    },
                );
                let rt = d.chain_output.clone();
                (d.chain_output.clone(), rt, !d.ok())
            };
            let notes_output = chain_output.clone();

            // ---- compact ----
            if let Some(comp) = &step.compact {
                if !errored && chain_output.chars().count() as u64 > comp.when_over {
                    if !opts.quiet {
                        eprintln!(
                            "sfh: [{}] compacting output ({} chars > {})",
                            step.id,
                            chain_output.chars().count(),
                            comp.when_over
                        );
                    }
                    total += 1;
                    // Keep what is about to be summarized: the chain file gets
                    // overwritten with the summary, and {{steps.X.outputs}} is
                    // documented as the pre-compact original - including after
                    // a --resume, which can only read files.
                    let pre_name = format!("{gtag}.precompact.txt");
                    let _ = contain::write_private(&run_dir.join(&pre_name), &chain_output);
                    log_event(
                        &mut log,
                        json!({"ts": utc_stamp(), "event": "compact_start", "step": step.id, "chars": chain_output.chars().count()}),
                    );
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
                        quiet: opts.quiet,
                        verbose: opts.verbose,
                    };
                    match run_compact(comp, compact_run) {
                        Ok((sum, usage)) => {
                            cost_usd += usage.cost_usd.unwrap_or(0.0);
                            log_event(
                                &mut log,
                                json!({"ts": utc_stamp(), "event": "compact_end", "step": step.id, "chars": sum.chars().count(), "cost_usd": usage.cost_usd, "precompact_file": pre_name}),
                            );
                            chain_output = sum.clone();
                            if let Some(e) = outputs.get_mut(&step.id) {
                                e.output = sum;
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "sfh: warning: step '{}' compact failed ({e}); using head+tail of the original",
                                step.id
                            );
                            log_event(
                                &mut log,
                                json!({"ts": utc_stamp(), "event": "compact_failed", "step": step.id, "error": e, "precompact_file": pre_name}),
                            );
                            chain_output = head_tail(&chain_output, comp.when_over as usize);
                            if let Some(en) = outputs.get_mut(&step.id) {
                                en.output = chain_output.clone();
                            }
                        }
                    }
                    let _ = contain::write_private(
                        &run_dir.join(format!("{gtag}.chain.txt")),
                        &chain_output,
                    );
                }
            }

            // A re-visited step writes <id>.v2.chain.txt, so the emitted file
            // has to follow the visit rather than assume <id>.chain.txt.
            chain_files.insert(step.id.clone(), run_dir.join(format!("{gtag}.chain.txt")));

            // ---- notes ----
            if step.notes.as_deref() == Some("append") && !errored {
                let mut f = contain::append_private(&notes_file)
                    .map_err(|e| format!("cannot open notes: {e}"))?;
                let _ = writeln!(
                    f,
                    "## {} (visit {visit})\n{}\n",
                    step.id,
                    notes_output.trim_end()
                );
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
                match step.on_error.as_deref().unwrap_or("fail") {
                    "continue" => {}
                    oe if oe.starts_with("goto:") => match &oe[5..] {
                        "end" => {
                            log_position(
                                &mut log,
                                &step.id,
                                "end".into(),
                                PositionVia::OnError,
                                None,
                            );
                            return Ok(FlowEnd::Completed);
                        }
                        "fail" => {
                            log_position(
                                &mut log,
                                &step.id,
                                "fail".into(),
                                PositionVia::OnError,
                                None,
                            );
                            return Err(format!(
                                "step '{}' failed and on_error routed to fail",
                                step.id
                            ));
                        }
                        "stuck" => {
                            log_position(
                                &mut log,
                                &step.id,
                                "stuck".into(),
                                PositionVia::OnError,
                                None,
                            );
                            return Ok(FlowEnd::Stuck {
                                after: step.id.clone(),
                            });
                        }
                        id => match index_of.get(id) {
                            Some(i) => {
                                log_position(
                                    &mut log,
                                    &step.id,
                                    id.to_string(),
                                    PositionVia::OnError,
                                    None,
                                );
                                cur = *i;
                                continue;
                            }
                            None => {
                                return Err(format!(
                                    "step '{}': on_error goto target '{id}' not found",
                                    step.id
                                ))
                            }
                        },
                    },
                    _ => {
                        return Err(format!(
                            "step '{}' failed - see {}",
                            step.id,
                            run_dir.display()
                        ))
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
                evaluate_route(step, &route_text, &ctx)?
            };
            match target.as_ref().map(|h| (h.goto.as_str(), h)) {
                None => {
                    log_position(
                        &mut log,
                        &step.id,
                        next_label(cur + 1, &flow),
                        PositionVia::Fallthrough,
                        None,
                    );
                    cur += 1;
                    if cur >= n_steps {
                        return Ok(FlowEnd::Completed);
                    }
                }
                Some(("end", hit)) => {
                    log_position(&mut log, &step.id, "end".into(), hit.via, Some(hit));
                    return Ok(FlowEnd::Completed);
                }
                Some(("fail", hit)) => {
                    log_position(&mut log, &step.id, "fail".into(), hit.via, Some(hit));
                    return Err(format!("step '{}' routed to fail", step.id));
                }
                Some(("stuck", hit)) => {
                    log_position(&mut log, &step.id, "stuck".into(), hit.via, Some(hit));
                    return Ok(FlowEnd::Stuck {
                        after: step.id.clone(),
                    });
                }
                Some((id, hit)) => {
                    if !opts.quiet {
                        eprintln!("sfh: [{}] -> goto {id}", step.id);
                    }
                    log_position(&mut log, &step.id, id.to_string(), hit.via, Some(hit));
                    cur = index_of[id];
                }
            }
        }
    })();

    let max_emit = flow.defaults.max_emit_chars.unwrap_or(200_000) as usize;
    let finish =
        |state: &'static str, cost: f64, code: i32, emit: Option<&str>, err: Option<&str>| {
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
            write_status(&status_path, &g);
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
            .filter(&nonempty)
            .or_else(|| last_success.clone().filter(&nonempty))
            .or_else(|| last_executed.clone().filter(&nonempty))
    };
    let mut meta_final = meta.clone();
    if let Some(m) = meta_final.as_object_mut() {
        m.insert("finished_utc".into(), json!(utc_stamp()));
        m.insert("leaf_runs".into(), json!(total));
        m.insert("cost_usd".into(), json!(cost_usd));
        m.insert(
            "status".into(),
            json!(match &result {
                Ok(FlowEnd::Completed) => "ok",
                Ok(FlowEnd::Stuck { .. }) => "stuck",
                Err(_) => "failed",
            }),
        );
    }
    let _ = contain::write_private(
        &run_dir.join("meta.json"),
        serde_json::to_string_pretty(&meta_final).unwrap_or_default(),
    );

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
        if let Some(id) = pick {
            if let Some(o) = outputs.get(id) {
                if !o.output.trim().is_empty() {
                    eprintln!("sfh: emitting partial result from step '{id}'");
                    print_emit(&o.output, max_emit, chain_files.get(id));
                }
            }
        }
    };

    match result {
        Ok(FlowEnd::Completed) => {
            log_event(
                &mut log,
                json!({"ts": utc_stamp(), "event": "run_end", "status": "ok", "leaf_runs": total, "cost_usd": cost_usd}),
            );
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
                print_emit(&out, max_emit, chain_files.get(id));
            } else if last_executed.is_none() {
                // Rare, but leaving status.json on "running" would make this
                // look like a run that got killed rather than one that ended.
                finish("failed", cost_usd, 1, None, Some("no step was executed"));
                return Err("no step was executed".into());
            }
            finish("done", cost_usd, 0, emit_id.as_deref(), None);
            if !opts.quiet {
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
                json!({"ts": utc_stamp(), "event": "run_end", "status": "stuck", "error": msg, "after": after, "leaf_runs": total, "cost_usd": cost_usd}),
            );
            eprintln!("sfh: FLOW STUCK: {msg}");
            // Emit before the status goes terminal, for the same reason the
            // success path does: `sfh wait` reads a terminal status.json as
            // "the output is ready".
            emit_partial(&partial_pick);
            finish("stuck", cost_usd, 4, partial_pick.as_deref(), Some(&msg));
            eprintln!("sfh: run dir: {}", run_dir.display());
            print_resume_hint();
            Ok(4)
        }
        Err(msg) => {
            log_event(
                &mut log,
                json!({"ts": utc_stamp(), "event": "run_end", "status": "failed", "error": msg, "leaf_runs": total, "cost_usd": cost_usd}),
            );
            eprintln!("sfh: FLOW FAILED: {msg}");
            emit_partial(&partial_pick);
            finish("failed", cost_usd, 1, partial_pick.as_deref(), Some(&msg));
            eprintln!("sfh: run dir: {}", run_dir.display());
            print_resume_hint();
            Ok(1)
        }
    }
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
        if !d.join("log.jsonl").exists() {
            return Err(format!("{} is not an sfh run directory", d.display()));
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
) {
    let gfile = run_dir.join(format!("{gtag}.out.txt"));
    let _ = contain::write_private(&gfile, agg);
    let _ = contain::write_private(&run_dir.join(format!("{gtag}.chain.txt")), agg);
    outputs.insert(
        step_id.to_string(),
        template::StepOutput {
            output: agg.to_string(),
            outputs: agg.to_string(),
            output_file: gfile.display().to_string(),
            exit: if failed { 1 } else { 0 },
            stderr_file: String::new(),
        },
    );
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

struct CompactRun<'a, 'ctx> {
    flow: &'a flow::Flow,
    ctx: &'a template::Ctx<'ctx>,
    original: &'a str,
    run_dir: &'a Path,
    tag: &'a str,
    /// The summarizer is a child of this run like any other, so its output
    /// counts as the run being alive.
    run_clock: &'a Arc<AtomicU64>,
    quiet: bool,
    verbose: bool,
}

fn run_compact(
    comp: &flow::Compact,
    run: CompactRun<'_, '_>,
) -> Result<(String, preset::Usage), String> {
    let CompactRun {
        flow,
        ctx,
        original,
        run_dir,
        tag,
        run_clock,
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
    let prep = leaf::Prepared {
        tag: ctag.clone(),
        inv: execute::Invocation::Argv(argv),
        parse: built.parse,
        stdin_payload,
        cwd: None,
        timeout: timeout_sec.map(Duration::from_secs),
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
        retry: leaf::RetryCfg::default(),
        run_clock: Some(Arc::clone(run_clock)),
        quiet,
        verbose,
    };
    let d = leaf::exec_leaf(prep);
    if !d.ok() {
        return Err(format!(
            "summarizer exit={} timed_out={}",
            d.exit_code, d.timed_out
        ));
    }
    let s = d.chain_output.trim().to_string();
    if s.is_empty() {
        return Err("summarizer returned empty output".into());
    }
    Ok((s, d.usage))
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
                Err(_) => {
                    let start = t.find('[').ok_or("foreach: no JSON array found in input")?;
                    let end = t
                        .rfind(']')
                        .ok_or("foreach: no JSON array found in input")?;
                    if end <= start {
                        return Err("foreach: no JSON array found in input".into());
                    }
                    serde_json::from_str(&t[start..=end])
                        .map_err(|e| format!("foreach: invalid JSON array: {e}"))?
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

fn dry_run(
    flow: &flow::Flow,
    vars: &BTreeMap<String, String>,
    tainted_vars: &HashSet<String>,
    run_dir: &Path,
    flow_dir: &Path,
    notes_file: &Path,
    needed_sessions: &HashSet<String>,
) -> Result<i32, String> {
    let step_ids = flow.step_ids();
    let outputs: BTreeMap<String, template::StepOutput> = BTreeMap::new();
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
    println!("dry run: steps in file order (routing not simulated)");
    println!("run dir (prompts rendered here): {}", run_dir.display());
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
        // Nothing has been spent and no time has passed, so {{budget.*}} shows
        // the whole declared budget - which is what a prompt reviewer wants to
        // see, and it exercises the `unlimited` spelling for undeclared axes.
        budget: leaf::BudgetVars::new(&flow.defaults, 0.0, 0),
        quiet: true,
        verbose: false,
    };
    for s in &flow.steps {
        println!("[{}] ({})", s.id, describe_kind(flow, s));
        if let Some(children) = &s.parallel {
            for c in children {
                let p = leaf::prepare_leaf(&cx, c, 1, &c.id, &[], None)?;
                println!("  child {}: {}", c.id, p.inv.describe());
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
        } else {
            let p = leaf::prepare_leaf(&cx, s, 1, &s.id, &[], None)?;
            println!("  cmd: {}", p.inv.describe());
            if let Some(t) = &s.continue_from {
                println!("  (resumes session of '{t}')");
            }
            for fb in &s.fallback {
                let p = leaf::prepare_leaf(&cx, s, 1, &format!("{}.fb", s.id), &[], Some(fb))?;
                println!("  fallback {fb}: {}", p.inv.describe());
            }
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
            let cond = if cond.is_empty() {
                "always".to_string()
            } else {
                cond.join(" && ")
            };
            println!("  route: {cond} -> {}", r.goto);
        }
        println!();
    }
    Ok(0)
}

fn log_event(f: &mut std::fs::File, v: serde_json::Value) {
    let _ = writeln!(f, "{v}");
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
) {
    if restored.is_empty() {
        return;
    }
    let mut names: Vec<&String> = restored.iter().collect();
    names.sort();
    log_event(
        f,
        json!({"ts": utc_stamp(), "event": "members_restored", "steps": names, "parent": group, "visit": visit}),
    );
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
const ROUTE_LINE_CHARS: usize = 200;

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
}

fn evaluate_route(
    step: &flow::Step,
    route_text: &str,
    ctx: &template::Ctx<'_>,
) -> Result<Option<RouteHit>, String> {
    let last = leaf::last_line(route_text).to_string();
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
        if matched {
            let via = if r.when_contains.is_none()
                && r.when_matches.is_none()
                && r.when_last_line_contains.is_none()
                && r.when_last_line_is.is_none()
                && r.when_last_line_matches.is_none()
            {
                PositionVia::CatchAll
            } else {
                PositionVia::Rule
            };
            return Ok(Some(RouteHit {
                goto: r.goto.clone(),
                via,
                rule: idx,
                line: route_line_of(r, route_text, &last),
                votes: None,
                voters: None,
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
) {
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
    }
    log_event(f, ev);
}

fn log_step_end(
    f: &mut std::fs::File,
    step: &str,
    parent: Option<&str>,
    visit: u32,
    d: &leaf::LeafDone,
) {
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
            "visit": visit, "exit": d.exit_code, "timed_out": d.timed_out,
            "interrupted": d.interrupted, "attempts": d.attempts, "dur_ms": d.dur_ms as u64,
            "idle_ms": d.idle_ms,
            "output_chars": d.chain_output.chars().count(),
            "output_hash": fingerprint(&d.chain_output),
            "input_tokens": d.usage.input_tokens, "output_tokens": d.usage.output_tokens,
            "cost_usd": d.usage.cost_usd, "tool": d.tool,
            "chain_file": file_name(&d.out_file).map(|n| n.replace(".out.txt", ".chain.txt")),
            "out_file": file_name(&d.out_file),
            "cmd": d.cmd, "session": session,
            // Which OS produced this step. A log is routinely read on a
            // different machine from the one that wrote it, and "it passes on
            // mine" is exactly the class of report this answers.
            "os": std::env::consts::OS,
        }),
    );
}

fn log_aggregate_end(
    f: &mut std::fs::File,
    step: &str,
    visit: u32,
    gtag: &str,
    failed: bool,
    plain: &str,
    plain_file: &str,
) {
    log_event(
        f,
        json!({
            "ts": utc_stamp(), "event": "aggregate_end", "step": step, "visit": visit,
            "failed": failed, "exit": if failed { 1 } else { 0 },
            "output_hash": fingerprint(plain),
            "chain_file": format!("{gtag}.chain.txt"), "out_file": format!("{gtag}.out.txt"),
            "plain_file": plain_file,
        }),
    );
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
}
