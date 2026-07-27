use crate::{execute, flow, leaf, preset, template};
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct RunOpts {
    pub flow_path: PathBuf,
    pub vars: Vec<(String, String)>,
    pub emit: Option<String>,
    pub runs_dir: Option<PathBuf>,
    pub dry_run: bool,
    pub verbose: bool,
    pub quiet: bool,
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
        return format!("parallel x{}", s.parallel.as_ref().map(|c| c.len()).unwrap_or(0));
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
        for k in ["run_dir", "flow_dir", "step_id", "visit", "os", "prompt_file", "notes"] {
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
        // foreach.from and route conditions render WITHOUT them at runtime,
        // so precheck must reject them there too.
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
            if let Some(c) = &r.when_contains {
                chk(&ctx_base, "route when_contains", c)?;
            }
            if let Some(c) = &r.when_matches {
                chk(&ctx_base, "route when_matches", c)?;
            }
        }
        if !s.is_group() {
            // Check the MERGED values (step > profile > defaults) so templates
            // supplied by a profile or defaults are validated too.
            let eff = leaf::effective(flow, s)?;
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
            if let (Some(tool), Some(e)) = (&eff.tool, &eff.effort) {
                if !e.contains("{{") {
                    if let Some(w) = effort_vocab_warning(tool, e) {
                        eprintln!("sfh: warning: step '{}': {w}", s.id);
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
        "codex" => &["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"],
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
    let runs_root = opts
        .runs_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(".sfh").join("runs"));
    let name = flow.name.clone().unwrap_or_else(|| "flow".into());
    let base = format!(
        "{}-{}{}",
        utc_stamp(),
        name,
        if opts.dry_run { "-dryrun" } else { "" }
    );
    let mut run_dir = runs_root.join(&base);
    let mut n = 1;
    while run_dir.exists() {
        n += 1;
        run_dir = runs_root.join(format!("{base}-{n}"));
    }
    std::fs::create_dir_all(&run_dir)
        .map_err(|e| format!("cannot create run dir {}: {e}", run_dir.display()))?;
    let run_dir = abs(&run_dir);
    let notes_file = run_dir.join("notes.md");

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

    if opts.dry_run {
        return dry_run(&flow, &vars, &run_dir, &flow_dir, &notes_file, &needed_sessions, opts);
    }

    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("log.jsonl"))
        .map_err(|e| format!("cannot open log: {e}"))?;
    let meta = json!({
        "flow": abs(&opts.flow_path).display().to_string(),
        "name": name,
        "started_utc": utc_stamp(),
        "vars": vars,
    });
    let _ = std::fs::write(
        run_dir.join("meta.json"),
        serde_json::to_string_pretty(&meta).unwrap_or_default(),
    );

    let index_of: HashMap<String, usize> = flow
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone(), i))
        .collect();
    let mut outputs: BTreeMap<String, template::StepOutput> = BTreeMap::new();
    let mut visits: HashMap<String, u32> = HashMap::new();
    let mut sessions: HashMap<String, leaf::SessionInfo> = HashMap::new();
    let mut last_executed: Option<String> = None;
    let mut cur = 0usize;
    let mut total: u32 = 0;
    let max_total = flow.defaults.max_total_steps.unwrap_or(100);
    let n_steps = flow.steps.len();

    let result: Result<(), String> = (|| {
        loop {
            let step = &flow.steps[cur];
            let visit = {
                let v = visits.entry(step.id.clone()).or_insert(0);
                *v += 1;
                *v
            };
            let max_v = step.max_visits.or(flow.defaults.max_visits).unwrap_or(5);
            if visit > max_v {
                return Err(format!(
                    "step '{}' exceeded max_visits ({max_v}) - loop not converging",
                    step.id
                ));
            }
            let gtag = if visit == 1 {
                step.id.clone()
            } else {
                format!("{}.v{visit}", step.id)
            };

            // ---- execute the step (leaf / parallel / foreach) ----
            // route_text: what route conditions match against - always the
            // pre-compact text, without sfh's "--- id ---" aggregate headers.
            let (mut chain_output, route_text, errored): (String, String, bool) =
                if let Some(children) = &step.parallel {
                    let cx = leaf::PrepCtx {
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
                    let mut preps = Vec::new();
                    for c in children {
                        let ctag = if visit == 1 {
                            c.id.clone()
                        } else {
                            format!("{}.v{visit}", c.id)
                        };
                        preps.push(leaf::prepare_leaf(&cx, c, visit, &ctag, &[])?);
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
                    log_event(&mut log, json!({"ts": utc_stamp(), "event": "group_start", "step": step.id, "visit": visit, "children": preps.len()}));
                    let dones = leaf::run_pool(preps, mp);
                    let mut agg = String::new();
                    let mut plain = String::new();
                    let mut hard_fail = false;
                    for (c, d) in children.iter().zip(dones.iter()) {
                        let ok = d.exit_code == 0 && !d.timed_out;
                        if !ok {
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
                        log_event(&mut log, json!({"ts": utc_stamp(), "event": "step_end", "step": c.id, "parent": step.id, "visit": visit, "exit": d.exit_code, "timed_out": d.timed_out, "dur_ms": d.dur_ms as u64, "output_chars": d.chain_output.chars().count()}));
                        agg.push_str(&format!("--- {} ---\n{}\n\n", c.id, d.chain_output.trim_end()));
                        plain.push_str(&format!("{}\n\n", d.chain_output.trim_end()));
                    }
                    let agg = agg.trim_end().to_string();
                    let plain = plain.trim_end().to_string();
                    let gfile = run_dir.join(format!("{gtag}.out.txt"));
                    let _ = std::fs::write(&gfile, &agg);
                    outputs.insert(
                        step.id.clone(),
                        template::StepOutput {
                            output: agg.clone(),
                            outputs: agg.clone(),
                            output_file: gfile.display().to_string(),
                        },
                    );
                    (agg, plain, hard_fail)
                } else if let Some(fe) = &step.foreach {
                    let cx = leaf::PrepCtx {
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
                    log_event(&mut log, json!({"ts": utc_stamp(), "event": "foreach_start", "step": step.id, "visit": visit, "items": items.len()}));
                    let dones = leaf::run_pool(preps, mp);
                    let mut agg = String::new();
                    let mut plain = String::new();
                    let mut any_fail = false;
                    for (i, d) in dones.iter().enumerate() {
                        let ok = d.exit_code == 0 && !d.timed_out;
                        if !ok {
                            any_fail = true;
                            eprintln!(
                                "sfh: [{}] failed (exit={}, timed_out={})",
                                d.tag, d.exit_code, d.timed_out
                            );
                            for line in leaf::tail_lines(&d.stderr_clean, 10) {
                                eprintln!("sfh: [{}] stderr| {line}", d.tag);
                            }
                        }
                        log_event(&mut log, json!({"ts": utc_stamp(), "event": "step_end", "step": format!("{}[{i}]", step.id), "visit": visit, "exit": d.exit_code, "timed_out": d.timed_out, "dur_ms": d.dur_ms as u64, "output_chars": d.chain_output.chars().count()}));
                        agg.push_str(&format!(
                            "--- {}[{i}] item: {} ---\n{}\n\n",
                            step.id,
                            one_line(&items[i], 80),
                            d.chain_output.trim_end()
                        ));
                        plain.push_str(&format!("{}\n\n", d.chain_output.trim_end()));
                    }
                    let agg = agg.trim_end().to_string();
                    let plain = plain.trim_end().to_string();
                    let gfile = run_dir.join(format!("{gtag}.out.txt"));
                    let _ = std::fs::write(&gfile, &agg);
                    outputs.insert(
                        step.id.clone(),
                        template::StepOutput {
                            output: agg.clone(),
                            outputs: agg.clone(),
                            output_file: gfile.display().to_string(),
                        },
                    );
                    (agg, plain, any_fail)
                } else {
                    if total + 1 > max_total {
                        return Err(format!("exceeded max_total_steps ({max_total})"));
                    }
                    total += 1;
                    let cx = leaf::PrepCtx {
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
                    let prep = leaf::prepare_leaf(&cx, step, visit, &gtag, &[])?;
                    log_event(&mut log, json!({"ts": utc_stamp(), "event": "step_start", "step": step.id, "visit": visit, "cmd": prep.inv.describe()}));
                    let d = leaf::exec_leaf(prep);
                    let ok = d.exit_code == 0 && !d.timed_out;
                    if !ok {
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
                            },
                        );
                    }
                    log_event(&mut log, json!({"ts": utc_stamp(), "event": "step_end", "step": step.id, "visit": visit, "exit": d.exit_code, "timed_out": d.timed_out, "dur_ms": d.dur_ms as u64, "output_chars": d.chain_output.chars().count()}));
                    outputs.insert(
                        step.id.clone(),
                        template::StepOutput {
                            output: d.chain_output.clone(),
                            outputs: d.chain_output.clone(),
                            output_file: d.out_file.display().to_string(),
                        },
                    );
                    let rt = d.chain_output.clone();
                    (d.chain_output, rt, !ok)
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
                    match run_compact(&flow, comp, &chain_output, &run_dir, &gtag, opts.quiet, opts.verbose) {
                        Ok(sum) => {
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
                            // Keep both ends: trailing verdict markers are the
                            // shipped convention, so a head-only cut would drop them.
                            let chars: Vec<char> = chain_output.chars().collect();
                            let n = comp.when_over as usize;
                            if chars.len() > n {
                                let half = (n / 2).max(1);
                                let head: String = chars[..half].iter().collect();
                                let tail: String = chars[chars.len() - half..].iter().collect();
                                chain_output =
                                    format!("{head}\n...[sfh: truncated middle]...\n{tail}");
                            }
                            if let Some(en) = outputs.get_mut(&step.id) {
                                en.output = chain_output.clone();
                            }
                        }
                    }
                }
            }

            // ---- notes ----
            if step.notes.as_deref() == Some("append") && !errored {
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&notes_file)
                    .map_err(|e| format!("cannot open notes: {e}"))?;
                let _ = writeln!(f, "## {} (visit {visit})\n{}\n", step.id, chain_output.trim_end());
            }

            last_executed = Some(step.id.clone());

            // ---- error handling ----
            if errored {
                match step.on_error.as_deref().unwrap_or("fail") {
                    "continue" => {}
                    oe if oe.starts_with("goto:") => match &oe[5..] {
                        "end" => return Ok(()),
                        "fail" => {
                            return Err(format!("step '{}' failed and on_error routed to fail", step.id))
                        }
                        id => match index_of.get(id) {
                            Some(i) => {
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
                let cx = leaf::PrepCtx {
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
                let builtins = leaf::make_builtins(&cx, &step.id, visit, &pf, &[]);
                let ctx = template::Ctx {
                    vars: &vars,
                    outputs: &outputs,
                    step_ids: &step_ids,
                    builtins,
                };
                for r in &step.route {
                    let mut ok = true;
                    if let Some(s) = &r.when_contains {
                        let s = template::render(s, &ctx)?;
                        if !route_text.contains(&s) {
                            ok = false;
                        }
                    }
                    if ok {
                        if let Some(rx) = &r.when_matches {
                            let rx_s = template::render(rx, &ctx)?;
                            let rx = regex::Regex::new(&rx_s)
                                .map_err(|e| format!("step '{}' route regex: {e}", step.id))?;
                            if !rx.is_match(&route_text) {
                                ok = false;
                            }
                        }
                    }
                    if ok {
                        target = Some(r.goto.clone());
                        break;
                    }
                }
            }
            match target.as_deref() {
                None => {
                    cur += 1;
                    if cur >= n_steps {
                        return Ok(());
                    }
                }
                Some("end") => return Ok(()),
                Some("fail") => return Err(format!("step '{}' routed to fail", step.id)),
                Some(id) => {
                    if !opts.quiet {
                        eprintln!("sfh: [{}] -> goto {id}", step.id);
                    }
                    cur = index_of[id];
                }
            }
        }
    })();

    match result {
        Ok(()) => {
            let emit_id = opts
                .emit
                .clone()
                .or(last_executed)
                .ok_or("no step was executed")?;
            let out = outputs
                .get(&emit_id)
                .map(|s| s.output.clone())
                .unwrap_or_default();
            println!("{out}");
            if !opts.quiet {
                eprintln!("sfh: done. run dir: {}", run_dir.display());
            }
            Ok(0)
        }
        Err(msg) => {
            eprintln!("sfh: FLOW FAILED: {msg}");
            eprintln!("sfh: run dir: {}", run_dir.display());
            Ok(1)
        }
    }
}

fn run_compact(
    flow: &flow::Flow,
    comp: &flow::Compact,
    original: &str,
    run_dir: &Path,
    tag: &str,
    quiet: bool,
    verbose: bool,
) -> Result<String, String> {
    let prof = comp.use_.as_ref().and_then(|u| flow.profiles.get(u));
    let tool = comp
        .tool
        .clone()
        .or_else(|| prof.and_then(|p| p.tool.clone()))
        .ok_or("compact: no tool resolved")?;
    let bin = comp.bin.clone().or_else(|| prof.and_then(|p| p.bin.clone()));
    let model = comp.model.clone().or_else(|| prof.and_then(|p| p.model.clone()));
    let effort = comp.effort.clone().or_else(|| prof.and_then(|p| p.effort.clone()));
    let target = comp.target_chars.unwrap_or(comp.when_over / 2).max(200);
    let instr = comp.instruction.clone().unwrap_or_else(|| {
        format!("以下のテキストを{target}文字以内に要約してください。後続のAIエージェントがコンテキストとして使います。重要な結論・数値・ファイルパス・未解決事項を必ず残し、要約本文のみを出力してください。")
    });
    let prompt = format!("{instr}\n\n---\n{original}");
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
    let built = preset::build(&tool, inp, &paths, None, false)?;
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
        session: preset::SessionCapture::Unsupported,
        expect_session: None,
        env_remove: built.env_remove,
        env_set: built.env_set,
        out_file: run_dir.join(format!("{ctag}.out.txt")),
        err_file: run_dir.join(format!("{ctag}.err.txt")),
        tool: Some(tool),
        quiet,
        verbose,
    };
    let d = leaf::exec_leaf(prep);
    if d.exit_code != 0 || d.timed_out {
        return Err(format!("summarizer exit={} timed_out={}", d.exit_code, d.timed_out));
    }
    let s = d.chain_output.trim().to_string();
    if s.is_empty() {
        return Err("summarizer returned empty output".into());
    }
    Ok(s)
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
                    let end = t.rfind(']').ok_or("foreach: no JSON array found in input")?;
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
            Ok(s
                .split(sep)
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
    opts: &RunOpts,
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
                let p = leaf::prepare_leaf(&cx, c, 1, &c.id, &[])?;
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
                &[("item", "<item>".to_string()), ("item_index", "0".to_string())],
            )?;
            println!("  cmd (per item): {}", p.inv.describe());
        } else {
            let p = leaf::prepare_leaf(&cx, s, 1, &s.id, &[])?;
            println!("  cmd: {}", p.inv.describe());
            if s.continue_from.is_some() {
                println!("  (resumes session of '{}')", s.continue_from.as_deref().unwrap());
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
            let cond = if cond.is_empty() {
                "always".to_string()
            } else {
                cond.join(" && ")
            };
            println!("  route: {cond} -> {}", r.goto);
        }
        println!();
    }
    let _ = opts;
    Ok(0)
}

fn log_event(f: &mut std::fs::File, v: serde_json::Value) {
    let _ = writeln!(f, "{v}");
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

fn utc_stamp() -> String {
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
