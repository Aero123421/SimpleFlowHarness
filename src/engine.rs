use crate::{contain, execute, flow, leaf, preset, template};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
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
        precheck(&flow, &vars)?;
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
/// any expensive agent runs. Also checks profile/access resolution.
fn precheck(flow: &flow::Flow, vars: &BTreeMap<String, String>) -> Result<(), String> {
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
        match &s.cmd {
            Some(flow::Cmd::Shell(c)) => chk(body_ctx, "cmd", c)?,
            Some(flow::Cmd::Argv(v)) => {
                for c in v {
                    chk(body_ctx, "cmd", c)?;
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
                    ("bin", &eff.bin),
                    ("cwd", &eff.cwd),
                ] {
                    if let Some(t) = v {
                        chk(body_ctx, label, t)?;
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
}

fn load_resume(run_dir: &Path) -> Result<ResumeState, String> {
    let log = std::fs::read_to_string(run_dir.join("log.jsonl"))
        .map_err(|e| format!("cannot read {}/log.jsonl: {e}", run_dir.display()))?;
    let mut st = ResumeState::default();
    let mut last_step: Option<String> = None;
    let mut unfinished: BTreeMap<(String, u32), UnfinishedStep> = BTreeMap::new();
    for line in log.lines() {
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
                        st.cost_usd += c;
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
                let (kind, members) = if ev == "group_start" {
                    ("parallel", "children")
                } else {
                    ("foreach", "items")
                };
                let n = v
                    .get(members)
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
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
            "step_end" | "aggregate_end" => {
                if ev == "step_end" {
                    st.total += 1;
                }
                if let Some(c) = v.get("cost_usd").and_then(|x| x.as_f64()) {
                    st.cost_usd += c;
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
                let ok = v.get("exit").and_then(|x| x.as_i64()).unwrap_or(0) == 0
                    && !v
                        .get("timed_out")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false)
                    && !v
                        .get("interrupted")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false)
                    && !v.get("failed").and_then(|x| x.as_bool()).unwrap_or(false);
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
                let exit = v.get("exit").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
                let timed_out = v
                    .get("timed_out")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                let exposed = if ev == "step_end" && !ok {
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
                let stderr_file = out_canon
                    .as_ref()
                    .map(|p| stderr_file_for(p).display().to_string())
                    .filter(|p| Path::new(p).exists())
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
                } else {
                    st.completed = false;
                    st.start = Some(next.to_string());
                }
            }
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
    Ok(st)
}

/// Newest run directory produced by THIS flow file. Several flows usually share
/// one runs root, so picking the newest directory overall would resume the
/// wrong run.
fn latest_run_dir(root: &Path, flow_path: &Path) -> Option<PathBuf> {
    let want = abs(flow_path).display().to_string();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("log.jsonl").exists())
        .filter(|p| {
            std::fs::read_to_string(p.join("meta.json"))
                .ok()
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
    // Bind the nonce to the child's pid BEFORE anything else touches the run
    // dir, so a `sfh stop` landing right after the detach already sees a
    // consistent (pid, nonce) pair. The child rewrites the same bytes with its
    // own pid - which is exactly d.pid - so there is no window of disagreement.
    contain::write_nonce(run_dir, d.pid, nonce)
        .map_err(|e| format!("cannot write the stop nonce: {e}"))?;
    // Seed status.json only if the child has not already written its own, so
    // `sfh status` has something to report either way and neither clobbers.
    if !status_path.exists() {
        write_status(
            &status_path,
            &Status {
                state: "running",
                step: String::new(),
                started: utc_stamp(),
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
            },
        );
    }
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
fn fingerprint(s: &str) -> String {
    crate::sha256::hex(s.as_bytes())
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
const FINGERPRINT_ALGO: &str = "sha256";
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
    let expected = match old_algo {
        FINGERPRINT_ALGO => fingerprint(flow_text),
        LEGACY_FINGERPRINT_ALGO => legacy_fingerprint_fnv(flow_text),
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
}

fn write_status(path: &Path, s: &Status) {
    let unfinished_step = s.unfinished_step.as_ref().map(|u| {
        json!({
            "step": u.step,
            "started_utc": u.started,
            "cmd": u.cmd,
            "will_rerun": true,
        })
    });
    let v = json!({
        "state": s.state,
        "current_step": s.step,
        "started_utc": s.started,
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
    });
    let text = serde_json::to_string_pretty(&v).unwrap_or_default();
    // Write-then-rename: `sfh status` and `sfh wait` poll this file every few
    // seconds, and a plain write lets them read a half-written document. Rename
    // is atomic on both platforms, so a reader sees the old or the new file and
    // never a torn one.
    let tmp = path.with_extension("json.tmp");
    if contain::write_private(&tmp, &text).is_ok() && std::fs::rename(&tmp, path).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(&tmp);
    let _ = contain::write_private(path, &text);
}

fn run_inner(opts: &RunOpts) -> Result<i32, String> {
    let flow = flow::load(&opts.flow_path)?;
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
    let runs_root = opts
        .runs_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(".sfh").join("runs"));
    // Same rule `sfh validate` enforces through flow::validate: only characters
    // that would affect the run dir PATH are forbidden (R-6).
    let name = flow.name.clone().unwrap_or_else(|| "flow".into());
    flow::validate_name(&name)?;

    // Resolve the resume target BEFORE the precheck and before anything
    // touches the runs root. A resumed run must see the variable values its
    // original attempt recorded in meta.json - without them, completed steps
    // ran under the old overrides while routing/foreach/prompts after the
    // resume render with defaults, a mixed execution. And an explicit
    // --resume of a dir elsewhere must not fail because the DEFAULT runs
    // root cannot be created here: a read-only checkout resuming a run dir
    // on writable storage is a legitimate use, and protecting a runs root
    // the resumed run will never write to is unrelated to its safety.
    let resume_dir: Option<PathBuf> = if opts.dry_run {
        None
    } else {
        resume_target(opts, &runs_root)?.map(|d| abs(&d))
    };
    if let Some(dir) = &resume_dir {
        let meta: serde_json::Value = std::fs::read_to_string(dir.join("meta.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(json!({}));
        if !opts.force_resume {
            check_flow_fingerprint(&meta, &flow_text, dir, &opts.flow_path)?;
        }
        // Recorded vars override the flow's defaults; an explicit --var on
        // THIS command overrides both (applied after this block).
        if let Some(obj) = meta.get("vars").and_then(|x| x.as_object()) {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    vars.insert(k.clone(), s.to_string());
                }
            }
        }
    }
    for (k, v) in &opts.vars {
        vars.insert(k.clone(), v.clone());
    }
    precheck(&flow, &vars)?;

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
    // the token to the owning pid (see contain::write_nonce), which is what
    // lets `sfh stop` refuse a status.json rewritten to point at another pid.
    let nonce = match std::env::var("SFH_NONCE") {
        Ok(n) if !n.trim().is_empty() => {
            // Consume it: a flow step that launches sfh itself must not carry
            // this run's nonce into the nested run.
            std::env::remove_var("SFH_NONCE");
            n.trim().to_string()
        }
        _ => contain::random_nonce(),
    };
    if !opts.detach {
        // The detaching parent writes the file itself, after the spawn, when
        // it knows the child's pid (in detach_run).
        contain::write_nonce(&run_dir, std::process::id(), &nonce)
            .map_err(|e| format!("cannot write the stop nonce: {e}"))?;
    }
    let notes_file = run_dir.join("notes.md");

    if opts.dry_run {
        return dry_run(
            &flow,
            &vars,
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
    let wall_deadline = flow
        .defaults
        .wall_clock_sec
        .map(|s| Instant::now() + Duration::from_secs(s));

    let result: Result<(), String> = (|| {
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
            match target.as_ref().map(|(target, via)| (target.as_str(), *via)) {
                None => {
                    log_position(
                        &mut log,
                        &step.id,
                        next_label(completed_idx + 1, &flow),
                        PositionVia::Fallthrough,
                    );
                    cur = completed_idx + 1;
                    if cur >= n_steps {
                        return Ok(());
                    }
                }
                Some(("end", via)) => {
                    log_position(&mut log, &step.id, "end".into(), via);
                    return Ok(());
                }
                Some(("fail", via)) => {
                    log_position(&mut log, &step.id, "fail".into(), via);
                    return Err(format!("step '{}' routed to fail", step.id));
                }
                Some((id, via)) => {
                    if !opts.quiet {
                        eprintln!("sfh: [{}] -> goto {id}", step.id);
                    }
                    log_position(&mut log, &step.id, id.to_string(), via);
                    cur = index_of[id];
                }
            }
        }
        loop {
            if execute::interrupted() {
                return Err("interrupted (Ctrl+C): child processes were terminated".into());
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
                        );
                        cur += 1;
                        if cur >= n_steps {
                            return Ok(());
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
                            );
                            return Ok(());
                        }
                        "fail" => {
                            log_position(
                                &mut log,
                                &step.id,
                                "fail".into(),
                                PositionVia::MaxVisits,
                            );
                            return Err(format!("step '{}' exhausted max_visits ({max_v})", step.id));
                        }
                        id => {
                            log_position(
                                &mut log,
                                &step.id,
                                id.to_string(),
                                PositionVia::MaxVisits,
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
                let cx = mk_cx!(&outputs, &sessions);
                let mut preps = Vec::new();
                for c in children {
                    let ctag = if visit == 1 {
                        c.id.clone()
                    } else {
                        format!("{}.v{visit}", c.id)
                    };
                    preps.push(leaf::prepare_leaf(&cx, c, visit, &ctag, &[], None)?);
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
                        "sfh: [{}] parallel: {} children, max_parallel={mp}",
                        step.id,
                        preps.len()
                    );
                }
                log_event(
                    &mut log,
                    json!({"ts": utc_stamp(), "event": "group_start", "step": step.id, "visit": visit, "children": preps.len()}),
                );
                let mut dones = leaf::run_pool(preps, mp, Arc::clone(&gate));
                for (ci, c) in children.iter().enumerate() {
                    let ctag = if visit == 1 {
                        c.id.clone()
                    } else {
                        format!("{}.v{visit}", c.id)
                    };
                    fan_fallback!(dones[ci], c, c.id, ctag, &c.fallback, &[]);
                }
                let mut agg = String::new();
                let mut plain = String::new();
                let mut hard_fail = false;
                for (c, d) in children.iter().zip(dones.iter()) {
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
                log_aggregate_end(&mut log, &step.id, visit, &gtag, hard_fail, &plain, &plain_name);
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
                let mut preps = Vec::new();
                for (i, it) in items.iter().enumerate() {
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
                }
                if total + items.len() as u32 > max_total {
                    return Err(format!(
                            "step '{}' would bring total leaf runs to {} over max_total_steps ({max_total})",
                            step.id,
                            total + items.len() as u32
                        ));
                }
                total += items.len() as u32;
                let mp = step
                    .max_parallel
                    .or(flow.defaults.max_parallel)
                    .unwrap_or(4) as usize;
                if !opts.quiet {
                    eprintln!(
                        "sfh: [{}] foreach: {} items, max_parallel={mp}",
                        step.id,
                        items.len()
                    );
                }
                log_event(
                    &mut log,
                    json!({"ts": utc_stamp(), "event": "foreach_start", "step": step.id, "visit": visit, "items": items.len()}),
                );
                let mut dones = leaf::run_pool(preps, mp, Arc::clone(&gate));
                #[allow(clippy::needless_range_loop)]
                for i in 0..dones.len() {
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
                    fan_fallback!(dones[i], step, label, tag, &step.fallback, &extra);
                }
                let mut agg = String::new();
                let mut plain = String::new();
                let mut any_fail = false;
                for (i, d) in dones.iter().enumerate() {
                    let label = format!("{}[{i}]", step.id);
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
                log_aggregate_end(&mut log, &step.id, visit, &gtag, hard_fail, &plain, &plain_name);
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
                        json!({"ts": utc_stamp(), "event": "step_start", "step": step.id, "visit": visit, "cmd": prep.inv.describe()}),
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
                            log_position(&mut log, &step.id, "end".into(), PositionVia::OnError);
                            return Ok(());
                        }
                        "fail" => {
                            log_position(&mut log, &step.id, "fail".into(), PositionVia::OnError);
                            return Err(format!(
                                "step '{}' failed and on_error routed to fail",
                                step.id
                            ));
                        }
                        id => match index_of.get(id) {
                            Some(i) => {
                                log_position(
                                    &mut log,
                                    &step.id,
                                    id.to_string(),
                                    PositionVia::OnError,
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
            match target.as_ref().map(|(target, via)| (target.as_str(), *via)) {
                None => {
                    log_position(
                        &mut log,
                        &step.id,
                        next_label(cur + 1, &flow),
                        PositionVia::Fallthrough,
                    );
                    cur += 1;
                    if cur >= n_steps {
                        return Ok(());
                    }
                }
                Some(("end", via)) => {
                    log_position(&mut log, &step.id, "end".into(), via);
                    return Ok(());
                }
                Some(("fail", via)) => {
                    log_position(&mut log, &step.id, "fail".into(), via);
                    return Err(format!("step '{}' routed to fail", step.id));
                }
                Some((id, via)) => {
                    if !opts.quiet {
                        eprintln!("sfh: [{}] -> goto {id}", step.id);
                    }
                    log_position(&mut log, &step.id, id.to_string(), via);
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
    let mut meta_final = meta.clone();
    if let Some(m) = meta_final.as_object_mut() {
        m.insert("finished_utc".into(), json!(utc_stamp()));
        m.insert("leaf_runs".into(), json!(total));
        m.insert("cost_usd".into(), json!(cost_usd));
        m.insert(
            "status".into(),
            json!(if result.is_ok() { "ok" } else { "failed" }),
        );
    }
    let _ = contain::write_private(
        &run_dir.join("meta.json"),
        serde_json::to_string_pretty(&meta_final).unwrap_or_default(),
    );

    match result {
        Ok(()) => {
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
        Err(msg) => {
            log_event(
                &mut log,
                json!({"ts": utc_stamp(), "event": "run_end", "status": "failed", "error": msg, "leaf_runs": total, "cost_usd": cost_usd}),
            );
            eprintln!("sfh: FLOW FAILED: {msg}");
            // Hand the caller whatever finished work exists - a failed run is
            // exactly when the parent agent most needs something to act on.
            let pick = if opts.no_partial_emit {
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
                    .or_else(|| last_success.filter(&nonempty))
                    .or_else(|| last_executed.filter(&nonempty))
            };
            if let Some(id) = &pick {
                if let Some(o) = outputs.get(id) {
                    if !o.output.trim().is_empty() {
                        eprintln!("sfh: emitting partial result from step '{id}'");
                        print_emit(&o.output, max_emit, chain_files.get(id));
                    }
                }
            }
            finish("failed", cost_usd, 1, pick.as_deref(), Some(&msg));
            eprintln!("sfh: run dir: {}", run_dir.display());
            // Paths are quoted (flow names may carry spaces since R-6, and so
            // may the runs dir) and this attempt's --var overrides are
            // repeated, so the printed command works when pasted back even on
            // a resume that predates meta.json var restoration.
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
        out_file: run_dir.join(format!("{ctag}.out.txt")),
        err_file: run_dir.join(format!("{ctag}.err.txt")),
        chain_file: run_dir.join(format!("{ctag}.chain.txt")),
        tool: Some(tool),
        access: Some(preset::Access::Read),
        allow_empty: false,
        retry: leaf::RetryCfg::default(),
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
                            },
                        );
                    }
                }
            }
        }
    }
    println!("dry run: steps in file order (routing not simulated)");
    println!("run dir (prompts rendered here): {}\n", run_dir.display());
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

#[derive(Clone, Copy)]
enum PositionVia {
    Rule,
    CatchAll,
    Fallthrough,
    OnError,
    MaxVisits,
}

impl PositionVia {
    fn as_str(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::CatchAll => "catch_all",
            Self::Fallthrough => "fallthrough",
            Self::OnError => "on_error",
            Self::MaxVisits => "max_visits",
        }
    }
}

fn evaluate_route(
    step: &flow::Step,
    route_text: &str,
    ctx: &template::Ctx<'_>,
) -> Result<Option<(String, PositionVia)>, String> {
    let last = leaf::last_line(route_text).to_string();
    for r in &step.route {
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
            return Ok(Some((r.goto.clone(), via)));
        }
    }
    Ok(None)
}

fn log_position(f: &mut std::fs::File, after: &str, next: String, via: PositionVia) {
    log_event(
        f,
        json!({
            "ts": utc_stamp(), "event": "position", "after": after,
            "next": next, "via": via.as_str(),
        }),
    );
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
            "output_chars": d.chain_output.chars().count(),
            "output_hash": fingerprint(&d.chain_output),
            "input_tokens": d.usage.input_tokens, "output_tokens": d.usage.output_tokens,
            "cost_usd": d.usage.cost_usd, "tool": d.tool,
            "chain_file": file_name(&d.out_file).map(|n| n.replace(".out.txt", ".chain.txt")),
            "out_file": file_name(&d.out_file),
            "cmd": d.cmd, "session": session,
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
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
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
