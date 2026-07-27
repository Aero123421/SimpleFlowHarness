use crate::{execute, flow, leaf, preset, template};
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};
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
                for a in &eff.args {
                    chk(body_ctx, "args", a)?;
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
        _ => return None, // opencode: per-model variants
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

#[derive(Default)]
struct ResumeState {
    outputs: BTreeMap<String, template::StepOutput>,
    visits: HashMap<String, u32>,
    sessions: HashMap<String, leaf::SessionInfo>,
    total: u32,
    cost_usd: f64,
    start: Option<String>,
    completed: bool,
}

fn load_resume(run_dir: &Path) -> Result<ResumeState, String> {
    let log = std::fs::read_to_string(run_dir.join("log.jsonl"))
        .map_err(|e| format!("cannot read {}/log.jsonl: {e}", run_dir.display()))?;
    let mut st = ResumeState::default();
    let mut last_step: Option<String> = None;
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
            "step_end" | "aggregate_end" => {
                if ev == "step_end" {
                    st.total += 1;
                }
                if let Some(c) = v.get("cost_usd").and_then(|x| x.as_f64()) {
                    st.cost_usd += c;
                }
                let visit = v.get("visit").and_then(|x| x.as_u64()).unwrap_or(1) as u32;
                let is_child = v.get("parent").is_some();
                if !is_child {
                    let e = st.visits.entry(step.clone()).or_insert(0);
                    *e = (*e).max(visit);
                    last_step = Some(step.clone());
                }
                let ok = v.get("exit").and_then(|x| x.as_i64()).unwrap_or(0) == 0
                    && !v
                        .get("timed_out")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false)
                    && !v.get("failed").and_then(|x| x.as_bool()).unwrap_or(false);
                if ok {
                    let rd = |k: &str| -> Option<String> {
                        v.get(k)
                            .and_then(|x| x.as_str())
                            .and_then(|p| std::fs::read_to_string(run_dir.join(p)).ok())
                    };
                    let chain = rd("chain_file").unwrap_or_default();
                    let outs = rd("out_file").unwrap_or_else(|| chain.clone());
                    let file = v
                        .get("out_file")
                        .and_then(|x| x.as_str())
                        .map(|p| run_dir.join(p).display().to_string())
                        .unwrap_or_default();
                    st.outputs.insert(
                        step.clone(),
                        template::StepOutput {
                            output: chain.trim_end().to_string(),
                            outputs: outs.trim_end().to_string(),
                            output_file: file,
                        },
                    );
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
                                },
                            );
                        }
                    }
                }
            }
            "position" => {
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
    if st.start.is_none() && !st.completed {
        // The last step never produced a routing decision: re-run it.
        st.start = last_step;
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

/// Cheap change-detection fingerprint (FNV-1a); not a security hash.
fn fingerprint(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

struct Status {
    state: &'static str,
    step: String,
    started: String,
    steps_done: u32,
    cost_usd: f64,
    run_dir: String,
}

fn write_status(path: &Path, s: &Status) {
    let v = json!({
        "state": s.state,
        "current_step": s.step,
        "started_utc": s.started,
        "heartbeat_utc": utc_stamp(),
        "steps_done": s.steps_done,
        "cost_usd": s.cost_usd,
        "run_dir": s.run_dir,
        "pid": std::process::id(),
        "sfh_version": VERSION,
    });
    let _ = std::fs::write(path, serde_json::to_string_pretty(&v).unwrap_or_default());
}

fn run_inner(opts: &RunOpts) -> Result<i32, String> {
    let flow = flow::load(&opts.flow_path)?;
    let mut vars = flow.vars_string_map()?;
    for (k, v) in &opts.vars {
        vars.insert(k.clone(), v.clone());
    }
    precheck(&flow, &vars)?;
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
    let name = flow.name.clone().unwrap_or_else(|| "flow".into());

    // Which steps must produce resumable sessions (continue_from targets).
    let mut needed_sessions: HashSet<String> = HashSet::new();
    for s in &flow.steps {
        if let Some(t) = &s.continue_from {
            needed_sessions.insert(t.clone());
        }
        if let Some(children) = &s.parallel {
            for c in children {
                if let Some(t) = &c.continue_from {
                    needed_sessions.insert(t.clone());
                }
            }
        }
    }

    // ---- pick or create the run directory ----
    let mut resumed = ResumeState::default();
    let run_dir: PathBuf;
    let mut is_resume = false;
    if opts.dry_run {
        let base = format!("{}-{}-dryrun", utc_stamp(), name);
        run_dir = abs(&runs_root.join(base));
        std::fs::create_dir_all(&run_dir)
            .map_err(|e| format!("cannot create run dir {}: {e}", run_dir.display()))?;
    } else if let Some(dir) = resume_target(opts, &runs_root)? {
        let dir = abs(&dir);
        let meta: serde_json::Value = std::fs::read_to_string(dir.join("meta.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(json!({}));
        let old_fp = meta
            .get("flow_fingerprint")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if old_fp != flow_fp && !opts.force_resume {
            return Err(format!(
                "{} was produced by a different version of {} (use --force-resume to override)",
                dir.display(),
                opts.flow_path.display()
            ));
        }
        resumed = load_resume(&dir)?;
        if resumed.completed {
            return Err(format!(
                "{} already completed - nothing to resume",
                dir.display()
            ));
        }
        if resumed.start.is_none() {
            return Err(format!(
                "{}: cannot tell where to resume from",
                dir.display()
            ));
        }
        run_dir = dir;
        is_resume = true;
    } else {
        let base = format!("{}-{}", utc_stamp(), name);
        let mut d = runs_root.join(&base);
        let mut n = 1;
        while d.exists() {
            n += 1;
            d = runs_root.join(format!("{base}-{n}"));
        }
        std::fs::create_dir_all(&d)
            .map_err(|e| format!("cannot create run dir {}: {e}", d.display()))?;
        run_dir = abs(&d);
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

    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("log.jsonl"))
        .map_err(|e| format!("cannot open log: {e}"))?;

    // Provenance: which sfh, which tool builds. Cheap, no AI calls.
    let mut tool_versions = serde_json::Map::new();
    if !is_resume {
        for t in flow.tools_used() {
            let bin = flow
                .profiles
                .values()
                .find(|p| p.tool.as_deref() == Some(t.as_str()) && p.bin.is_some())
                .and_then(|p| p.bin.clone())
                .unwrap_or_else(|| t.clone());
            if let Some(v) = execute::probe_version(&bin) {
                tool_versions.insert(t.clone(), json!({"bin": bin, "version": v}));
            } else {
                tool_versions.insert(t.clone(), json!({"bin": bin, "version": null}));
            }
        }
    }
    let started = utc_stamp();
    let meta = json!({
        "sfh_version": VERSION,
        "flow": abs(&opts.flow_path).display().to_string(),
        "flow_fingerprint": flow_fp,
        "name": name,
        "started_utc": started,
        "os": std::env::consts::OS,
        "vars": vars,
        "tools": tool_versions,
        "resumed": is_resume,
    });
    let _ = std::fs::write(
        run_dir.join("meta.json"),
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
        step: resumed.start.clone().unwrap_or_default(),
        started: started.clone(),
        steps_done: resumed.total,
        cost_usd: resumed.cost_usd,
        run_dir: run_dir.display().to_string(),
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
    let mut last_executed: Option<String> = None;
    let mut last_success: Option<String> = None;
    let mut cur = match &resumed.start {
        Some(id) => *index_of
            .get(id)
            .ok_or_else(|| format!("resume: step '{id}' no longer exists in the flow"))?,
        None => 0,
    };
    if is_resume && !opts.quiet {
        eprintln!(
            "sfh: resuming {} at step '{}' ({} steps already done, ${cost_usd:.4} spent)",
            run_dir.display(),
            resumed.start.clone().unwrap_or_default(),
            total
        );
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
                        log_position(&mut log, &step.id, next_label(cur + 1, &flow));
                        cur += 1;
                        if cur >= n_steps {
                            return Ok(());
                        }
                        continue;
                    }
                    a if a.starts_with("goto:") => match &a[5..] {
                        "end" => {
                            log_position(&mut log, &step.id, "end".into());
                            return Ok(());
                        }
                        "fail" => {
                            log_position(&mut log, &step.id, "fail".into());
                            return Err(format!("step '{}' exhausted max_visits ({max_v})", step.id));
                        }
                        id => {
                            log_position(&mut log, &step.id, id.to_string());
                            cur = index_of[id];
                            continue;
                        }
                    },
                    _ => {
                        return Err(format!(
                            "step '{}' exceeded max_visits ({max_v}) - loop not converging (set on_max_visits: goto:<id> to degrade gracefully)",
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
                let dones = leaf::run_pool(preps, mp, Arc::clone(&gate));
                let mut agg = String::new();
                let mut plain = String::new();
                let mut hard_fail = false;
                for (c, d) in children.iter().zip(dones.iter()) {
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
                            output: d.chain_output.clone(),
                            outputs: d.chain_output.clone(),
                            output_file: d.out_file.display().to_string(),
                        },
                    );
                    if let (Some(tool), Some(sid)) = (&d.tool, &d.session_id) {
                        sessions.insert(
                            c.id.clone(),
                            leaf::SessionInfo {
                                tool: tool.clone(),
                                id: sid.clone(),
                                cwd: d.cwd.clone(),
                            },
                        );
                    }
                    log_step_end(&mut log, &c.id, Some(&step.id), visit, d);
                    agg.push_str(&format!(
                        "--- {} ---\n{}\n\n",
                        c.id,
                        d.chain_output.trim_end()
                    ));
                    plain.push_str(&format!("{}\n\n", d.chain_output.trim_end()));
                }
                let agg = agg.trim_end().to_string();
                let plain = plain.trim_end().to_string();
                write_aggregate(&run_dir, &gtag, &agg, &mut outputs, &step.id);
                log_aggregate_end(&mut log, &step.id, visit, &gtag, hard_fail);
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
                let dones = leaf::run_pool(preps, mp, Arc::clone(&gate));
                let mut agg = String::new();
                let mut plain = String::new();
                let mut any_fail = false;
                for (i, d) in dones.iter().enumerate() {
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
                    log_step_end(
                        &mut log,
                        &format!("{}[{i}]", step.id),
                        Some(&step.id),
                        visit,
                        d,
                    );
                    agg.push_str(&format!(
                        "--- {}[{i}] item: {} ---\n{}\n\n",
                        step.id,
                        one_line(items.get(i).map(String::as_str).unwrap_or(""), 80),
                        d.chain_output.trim_end()
                    ));
                    plain.push_str(&format!("{}\n\n", d.chain_output.trim_end()));
                }
                // A single failed item is fatal unless the step opted out.
                let hard_fail = any_fail && step.on_error.as_deref() != Some("continue");
                let agg = agg.trim_end().to_string();
                let plain = plain.trim_end().to_string();
                write_aggregate(&run_dir, &gtag, &agg, &mut outputs, &step.id);
                log_aggregate_end(&mut log, &step.id, visit, &gtag, hard_fail);
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
                        },
                    );
                }
                log_step_end(&mut log, &step.id, None, visit, d);
                outputs.insert(
                    step.id.clone(),
                    template::StepOutput {
                        output: d.chain_output.clone(),
                        outputs: d.chain_output.clone(),
                        output_file: d.out_file.display().to_string(),
                    },
                );
                let rt = d.chain_output.clone();
                (d.chain_output.clone(), rt, !d.ok())
            };

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
                    log_event(
                        &mut log,
                        json!({"ts": utc_stamp(), "event": "compact_start", "step": step.id, "chars": chain_output.chars().count()}),
                    );
                    match run_compact(
                        &flow,
                        comp,
                        &chain_output,
                        &run_dir,
                        &gtag,
                        opts.quiet,
                        opts.verbose,
                    ) {
                        Ok((sum, usage)) => {
                            cost_usd += usage.cost_usd.unwrap_or(0.0);
                            log_event(
                                &mut log,
                                json!({"ts": utc_stamp(), "event": "compact_end", "step": step.id, "chars": sum.chars().count(), "cost_usd": usage.cost_usd}),
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
                                json!({"ts": utc_stamp(), "event": "compact_failed", "step": step.id, "error": e}),
                            );
                            chain_output = head_tail(&chain_output, comp.when_over as usize);
                            if let Some(en) = outputs.get_mut(&step.id) {
                                en.output = chain_output.clone();
                            }
                        }
                    }
                    let _ =
                        std::fs::write(run_dir.join(format!("{gtag}.chain.txt")), &chain_output);
                }
            }

            // ---- notes ----
            if step.notes.as_deref() == Some("append") && !errored {
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&notes_file)
                    .map_err(|e| format!("cannot open notes: {e}"))?;
                let _ = writeln!(
                    f,
                    "## {} (visit {visit})\n{}\n",
                    step.id,
                    chain_output.trim_end()
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
                            log_position(&mut log, &step.id, "end".into());
                            return Ok(());
                        }
                        "fail" => {
                            log_position(&mut log, &step.id, "fail".into());
                            return Err(format!(
                                "step '{}' failed and on_error routed to fail",
                                step.id
                            ));
                        }
                        id => match index_of.get(id) {
                            Some(i) => {
                                log_position(&mut log, &step.id, id.to_string());
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
            let mut target: Option<String> = None;
            {
                let pf = run_dir.join(format!("{gtag}.prompt.txt"));
                let cx = mk_cx!(&outputs, &sessions);
                let builtins = leaf::make_builtins(&cx, &step.id, visit, &pf, &[]);
                let ctx = template::Ctx {
                    vars: &vars,
                    outputs: &outputs,
                    step_ids: &step_ids,
                    builtins,
                };
                let last = leaf::last_line(&route_text).to_string();
                for r in &step.route {
                    let mut ok = true;
                    let mut check =
                        |needle: &Option<String>, hay: &str, is_rx: bool| -> Result<(), String> {
                            if !ok {
                                return Ok(());
                            }
                            let Some(t) = needle else { return Ok(()) };
                            let t = template::render(t, &ctx)?;
                            let hit = if is_rx {
                                regex::Regex::new(&t)
                                    .map_err(|e| format!("step '{}' route regex: {e}", step.id))?
                                    .is_match(hay)
                            } else {
                                hay.contains(&t)
                            };
                            if !hit {
                                ok = false;
                            }
                            Ok(())
                        };
                    check(&r.when_contains, &route_text, false)?;
                    check(&r.when_matches, &route_text, true)?;
                    check(&r.when_last_line_contains, &last, false)?;
                    check(&r.when_last_line_matches, &last, true)?;
                    if ok {
                        target = Some(r.goto.clone());
                        break;
                    }
                }
            }
            match target.as_deref() {
                None => {
                    log_position(&mut log, &step.id, next_label(cur + 1, &flow));
                    cur += 1;
                    if cur >= n_steps {
                        return Ok(());
                    }
                }
                Some("end") => {
                    log_position(&mut log, &step.id, "end".into());
                    return Ok(());
                }
                Some("fail") => {
                    log_position(&mut log, &step.id, "fail".into());
                    return Err(format!("step '{}' routed to fail", step.id));
                }
                Some(id) => {
                    if !opts.quiet {
                        eprintln!("sfh: [{}] -> goto {id}", step.id);
                    }
                    log_position(&mut log, &step.id, id.to_string());
                    cur = index_of[id];
                }
            }
        }
    })();

    let max_emit = flow.defaults.max_emit_chars.unwrap_or(200_000) as usize;
    let finish = |state: &'static str, cost: f64| {
        let mut g = status.lock().unwrap();
        g.state = state;
        g.steps_done = total;
        g.cost_usd = cost;
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
    let _ = std::fs::write(
        run_dir.join("meta.json"),
        serde_json::to_string_pretty(&meta_final).unwrap_or_default(),
    );

    match result {
        Ok(()) => {
            log_event(
                &mut log,
                json!({"ts": utc_stamp(), "event": "run_end", "status": "ok", "leaf_runs": total, "cost_usd": cost_usd}),
            );
            finish("done", cost_usd);
            let emit_id = opts
                .emit
                .clone()
                .or(last_executed)
                .ok_or("no step was executed")?;
            let out = outputs
                .get(&emit_id)
                .map(|s| s.output.clone())
                .unwrap_or_default();
            print_emit(&out, max_emit, &run_dir, &emit_id);
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
            finish("failed", cost_usd);
            eprintln!("sfh: FLOW FAILED: {msg}");
            // Hand the caller whatever finished work exists - a failed run is
            // exactly when the parent agent most needs something to act on.
            if !opts.no_partial_emit {
                let nonempty = |id: &String| {
                    outputs
                        .get(id)
                        .map(|o| !o.output.trim().is_empty())
                        .unwrap_or(false)
                };
                let pick = opts
                    .emit
                    .clone()
                    .filter(&nonempty)
                    .or_else(|| last_success.filter(&nonempty))
                    .or_else(|| last_executed.filter(&nonempty));
                if let Some(id) = pick {
                    if let Some(o) = outputs.get(&id) {
                        if !o.output.trim().is_empty() {
                            eprintln!("sfh: emitting partial result from step '{id}'");
                            print_emit(&o.output, max_emit, &run_dir, &id);
                        }
                    }
                }
            }
            eprintln!("sfh: run dir: {}", run_dir.display());
            eprintln!(
                "sfh: resume with: sfh run {} --resume {}",
                opts.flow_path.display(),
                run_dir.display()
            );
            Ok(1)
        }
    }
}

fn print_emit(out: &str, max: usize, run_dir: &Path, id: &str) {
    let n = out.chars().count();
    if n > max {
        let cut: String = out.chars().take(max).collect();
        println!(
            "{cut}\n[sfh: emit truncated at {max} of {n} chars; full text: {}]",
            run_dir.join(format!("{id}.chain.txt")).display()
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
) {
    let gfile = run_dir.join(format!("{gtag}.out.txt"));
    let _ = std::fs::write(&gfile, agg);
    let _ = std::fs::write(run_dir.join(format!("{gtag}.chain.txt")), agg);
    outputs.insert(
        step_id.to_string(),
        template::StepOutput {
            output: agg.to_string(),
            outputs: agg.to_string(),
            output_file: gfile.display().to_string(),
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

fn run_compact(
    flow: &flow::Flow,
    comp: &flow::Compact,
    original: &str,
    run_dir: &Path,
    tag: &str,
    quiet: bool,
    verbose: bool,
) -> Result<(String, preset::Usage), String> {
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
    let instr = comp.instruction.clone().unwrap_or_else(|| {
        format!(
            "Summarize the text below in at most {target} characters, in the same language as the text. \
             It will be passed to another AI agent as context, so keep every conclusion, number, file path and open question. \
             Output only the summary."
        )
    });
    let prompt = format!("{instr}\n\n---\n{body}");
    let ctag = format!("{tag}.compact");
    let prompt_file = run_dir.join(format!("{ctag}.prompt.txt"));
    std::fs::write(&prompt_file, &prompt).map_err(|e| e.to_string())?;
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
        env_remove: built.env_remove,
        env_set: built.env_set,
        out_file: run_dir.join(format!("{ctag}.out.txt")),
        err_file: run_dir.join(format!("{ctag}.err.txt")),
        chain_file: run_dir.join(format!("{ctag}.chain.txt")),
        tool: Some(tool),
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
    // Fake sessions so continue_from steps can render their resume command.
    for s in &flow.steps {
        let targets: Vec<&String> = std::iter::once(&s.continue_from)
            .chain(
                s.parallel
                    .iter()
                    .flat_map(|cs| cs.iter().map(|c| &c.continue_from)),
            )
            .flatten()
            .collect();
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

fn log_position(f: &mut std::fs::File, after: &str, next: String) {
    log_event(
        f,
        json!({"ts": utc_stamp(), "event": "position", "after": after, "next": next}),
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
        (Some(t), Some(id)) => json!({"tool": t, "id": id, "cwd": d.cwd}),
        _ => serde_json::Value::Null,
    };
    log_event(
        f,
        json!({
            "ts": utc_stamp(), "event": "step_end", "step": step, "parent": parent,
            "visit": visit, "exit": d.exit_code, "timed_out": d.timed_out,
            "interrupted": d.interrupted, "attempts": d.attempts, "dur_ms": d.dur_ms as u64,
            "output_chars": d.chain_output.chars().count(),
            "input_tokens": d.usage.input_tokens, "output_tokens": d.usage.output_tokens,
            "cost_usd": d.usage.cost_usd, "tool": d.tool,
            "chain_file": file_name(&d.out_file).map(|n| n.replace(".out.txt", ".chain.txt")),
            "out_file": file_name(&d.out_file),
            "cmd": d.cmd, "session": session,
        }),
    );
}

fn log_aggregate_end(f: &mut std::fs::File, step: &str, visit: u32, gtag: &str, failed: bool) {
    log_event(
        f,
        json!({
            "ts": utc_stamp(), "event": "aggregate_end", "step": step, "visit": visit,
            "failed": failed, "exit": if failed { 1 } else { 0 },
            "chain_file": format!("{gtag}.chain.txt"), "out_file": format!("{gtag}.out.txt"),
        }),
    );
}

fn file_name(p: &Path) -> Option<String> {
    p.file_name().map(|n| n.to_string_lossy().into_owned())
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
    fn fingerprint_detects_flow_edits() {
        assert_eq!(fingerprint("a"), fingerprint("a"));
        assert_ne!(fingerprint("a"), fingerprint("b"));
        assert_eq!(fingerprint("").len(), 16);
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
    }
}
