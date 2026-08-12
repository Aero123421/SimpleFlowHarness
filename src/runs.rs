//! `sfh runs` - browse and prune the evidence trail.

use crate::contain;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn run_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(canon_root) = root.canonicalize() else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = match std::fs::read_dir(root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            // Do not follow a symlink/junction planted in the runs root. This
            // matters especially to `runs clean`, which deletes these paths.
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .filter(|p| {
                p.canonicalize()
                    .map(|resolved| resolved.starts_with(&canon_root))
                    .unwrap_or(false)
            })
            // A fixed-name artifact is untrusted input too. Requiring a
            // contained, no-follow read prevents list/show/clean from treating
            // a log.jsonl symlink as evidence that this is an sfh run.
            .filter(|p| {
                contain::read_contained_opt(p, "log.jsonl")
                    .map(|log| log.is_some())
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    v.sort();
    v
}

fn read_json(dir: &Path, name: &str) -> Value {
    contain::read_contained_opt(dir, name)
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

fn meta(dir: &Path) -> Value {
    read_json(dir, "meta.json")
}

fn status(dir: &Path) -> Value {
    read_json(dir, "status.json")
}

fn get<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("-")
}

fn bare_json_error(as_json: bool, dir: &Path, message: &str) -> i32 {
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": false,
                "run_dir": dir.display().to_string(),
                "error": message,
            }))
            .unwrap_or_default()
        );
    } else {
        eprintln!("sfh: {message}");
    }
    2
}

#[derive(Default)]
struct StepAccumulator {
    exit: Option<i64>,
    ok: u64,
    failed: u64,
    visit: u64,
    repeat: u64,
    current_repeat: u64,
    last_hash: Option<String>,
    dur_ms: u64,
    output_chars: u64,
    cost_usd: f64,
}

impl StepAccumulator {
    fn record(&mut self, event: &Value) {
        let exit = event.get("exit").and_then(Value::as_i64);
        self.exit = exit;
        match exit {
            Some(0) => self.ok += 1,
            Some(_) => self.failed += 1,
            None => {}
        }
        self.visit = self
            .visit
            .max(event.get("visit").and_then(Value::as_u64).unwrap_or(0));
        self.dur_ms += event.get("dur_ms").and_then(Value::as_u64).unwrap_or(0);
        self.output_chars += event
            .get("output_chars")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.cost_usd += event.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);

        let Some(hash) = event.get("output_hash").and_then(Value::as_str) else {
            self.last_hash = None;
            self.current_repeat = 0;
            return;
        };
        if self.last_hash.as_deref() == Some(hash) {
            self.current_repeat += 1;
            self.repeat = self.repeat.max(self.current_repeat);
        } else {
            self.last_hash = Some(hash.to_string());
            self.current_repeat = 0;
        }
    }
}

#[derive(Serialize)]
struct StepSummary {
    step: String,
    exit: Option<i64>,
    ok: u64,
    failed: u64,
    visit: u64,
    repeat: u64,
    dur_ms: u64,
    output_chars: u64,
    cost_usd: f64,
}

impl StepSummary {
    fn from_accumulator(step: String, a: StepAccumulator) -> Self {
        Self {
            step,
            exit: a.exit,
            ok: a.ok,
            failed: a.failed,
            visit: a.visit,
            repeat: a.repeat,
            dur_ms: a.dur_ms,
            output_chars: a.output_chars,
            cost_usd: a.cost_usd,
        }
    }

    fn use_aggregate_facts(&mut self, aggregate: StepAccumulator) {
        self.exit = aggregate.exit;
        self.ok = aggregate.ok;
        self.failed = aggregate.failed;
        self.visit = aggregate.visit;
        self.repeat = aggregate.repeat;
    }
}

/// P1-08: `cost_usd` used to be the only cost field a row reported, and it
/// answers a specific question - "what is this run's position against
/// max_cost_usd" - that is NOT the same as "what did this run itself
/// spend" the moment `--carry-budget-from` is involved. Four questions,
/// four fields, each named for exactly what it answers:
///
/// - `own_cost_usd`: what THIS run itself spent, carried inheritance
///   excluded.
/// - `carried_cost_usd`: what it inherited from an earlier run via
///   `--carry-budget-from`, spent by neither this run nor computed by it.
/// - `budget_position_usd`: own + carried - what `max_cost_usd` is actually
///   judged against, i.e. the same number `cost_usd` has always reported.
/// - `lineage_cost_usd`: the total spent across this run's WHOLE carry
///   ancestry, back to a run that carried nothing - but ONLY when every
///   ancestor in that chain is still present and readable. `runs clean`
///   deletes old run dirs; a descendant's `carried_budget.cost_usd` survives
///   that deletion (it was captured durably at carry time), so
///   `budget_position_usd` stays correct, but nothing can re-verify an
///   ancestor that is gone - so this is `None` rather than a value nothing
///   backs, instead of quietly reusing `budget_position_usd` and calling it
///   a lineage total it may not be.
///
/// `cost_usd` is kept, identical to `budget_position_usd`, so an existing
/// JSON consumer reading `.cost_usd` keeps getting the same number it
/// always did; new callers get the labelled fields instead of having to
/// guess which quantity `cost_usd` was.
#[derive(Serialize)]
struct RunSummary {
    run_dir: String,
    status: Option<String>,
    started_utc: Option<String>,
    exit: Option<i64>,
    ok: u64,
    failed: u64,
    visit: u64,
    repeat: u64,
    /// Kept for backward compatibility with existing JSON consumers;
    /// identical to `budget_position_usd`. New code should read the
    /// labelled fields below instead - this one does not say which of the
    /// four quantities it is.
    cost_usd: f64,
    own_cost_usd: f64,
    /// How much of `budget_position_usd` this run INHERITED from an earlier
    /// one via `--carry-budget-from` rather than spending itself. Zero for
    /// the ordinary run that carried nothing. Never negative and never
    /// larger than the run's own total: a hand-edited meta.json must not be
    /// able to turn a fleet total into a refund (see `summary`'s clamp).
    carried_cost_usd: f64,
    budget_position_usd: f64,
    /// `None` when this run's carry ancestry cannot be fully verified right
    /// now (an ancestor run dir was cleaned, or its meta.json cannot be
    /// read) - see the struct doc comment. Never a partial sum standing in
    /// for the real total.
    lineage_cost_usd: Option<f64>,
}

#[derive(Serialize)]
struct RunDetails {
    #[serde(flatten)]
    summary: RunSummary,
    flow: Option<String>,
    sfh_version: Option<String>,
    tools: Value,
    /// The `on_budget` landing, if this run spent one. Absent (null) is the
    /// normal case and means the run never came within its reserve of a
    /// ceiling - not that no budget was declared.
    budget_landed: Option<BudgetLanding>,
    steps: Vec<StepSummary>,
}

#[derive(Serialize)]
struct BudgetLanding {
    /// "cost" or "wall_clock" - which axis crossed its threshold.
    trigger: String,
    spent_usd: f64,
    elapsed_sec: u64,
    goto: String,
}

/// The landing event, read straight out of the log. First one wins: a run gets
/// one landing, and if a tampered log carries two, reporting the first is the
/// same "earliest recorded fact" rule the rest of the resume path uses.
fn budget_landing(dir: &Path) -> Option<BudgetLanding> {
    let log = contain::read_contained_opt(dir, "log.jsonl")
        .ok()
        .flatten()?;
    for line in log.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if event.get("event").and_then(Value::as_str) != Some("budget_landing") {
            continue;
        }
        return Some(BudgetLanding {
            trigger: event
                .get("trigger")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            spent_usd: event
                .get("spent_usd")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            elapsed_sec: event
                .get("elapsed_sec")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            goto: event
                .get("goto")
                .and_then(Value::as_str)
                .unwrap_or("-")
                .to_string(),
        });
    }
    None
}

fn step_summaries(dir: &Path) -> Vec<StepSummary> {
    let log = contain::read_contained_opt(dir, "log.jsonl")
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut leaves: BTreeMap<String, StepAccumulator> = BTreeMap::new();
    let mut aggregates: BTreeMap<String, StepAccumulator> = BTreeMap::new();
    for line in log.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = event.get("event").and_then(Value::as_str).unwrap_or("");
        if kind != "step_end" && kind != "aggregate_end" {
            continue;
        }
        let Some(step) = event.get("step").and_then(Value::as_str) else {
            continue;
        };
        let target = if kind == "aggregate_end" {
            &mut aggregates
        } else {
            &mut leaves
        };
        target.entry(step.to_string()).or_default().record(&event);
    }
    let mut steps: BTreeMap<String, StepSummary> = leaves
        .into_iter()
        .map(|(step, a)| (step.clone(), StepSummary::from_accumulator(step, a)))
        .collect();
    for (step, aggregate) in aggregates {
        if let Some(existing) = steps.get_mut(&step) {
            existing.use_aggregate_facts(aggregate);
        } else {
            steps.insert(step.clone(), StepSummary::from_accumulator(step, aggregate));
        }
    }
    steps.into_values().collect()
}

fn opt_string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// The maximum number of `--carry-budget-from` hops `lineage_is_resolvable`
/// walks before giving up. A real ancestry is never remotely this long;
/// hitting the bound means `carried_budget.from` cycles back on itself
/// (by accident, or by a hand-edited meta.json) rather than naming a real
/// ancestor, so it is itself treated as proof the chain cannot be trusted.
const MAX_LINEAGE_HOPS: usize = 10_000;

/// Whether `dir`'s complete `--carry-budget-from` ancestry - back to a run
/// that carried nothing - is still present and its meta.json still
/// readable, hop by hop. `runs clean` deletes old run dirs; this is what
/// lets `lineage_cost_usd` notice when that has happened to one of THIS
/// run's ancestors, rather than silently reporting a total that stopped
/// being verifiable the moment the evidence for one hop was gone.
fn lineage_is_resolvable(dir: &Path) -> bool {
    let mut current = dir.to_path_buf();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..MAX_LINEAGE_HOPS {
        // A dir this walk has already visited means `carried_budget.from`
        // cycles back on itself - not a real ancestry, and not something to
        // report a total for.
        if !seen.insert(current.clone()) {
            return false;
        }
        let m = meta(&current);
        if m.is_null() {
            return false;
        }
        match m
            .get("carried_budget")
            .and_then(|c| c.get("from"))
            .and_then(Value::as_str)
        {
            Some(from) => current = PathBuf::from(from),
            // An origin that carried nothing: the chain is complete.
            None => return true,
        }
    }
    false
}

fn summary(dir: &Path, steps: &[StepSummary]) -> RunSummary {
    let m = meta(dir);
    let s = status(dir);
    // status.json is the live/final state contract. Prefer it over meta.json
    // so a failure while persisting the terminal heartbeat cannot be masked by
    // metadata that was written slightly earlier.
    let status = opt_string(&s, "state").or_else(|| opt_string(&m, "status"));
    let exit = s.get("exit_code").and_then(Value::as_i64);
    let ok = steps.iter().map(|x| x.ok).sum();
    let failed = steps.iter().map(|x| x.failed).sum();
    let visit = steps.iter().map(|x| x.visit).max().unwrap_or(0);
    let repeat = steps.iter().map(|x| x.repeat).max().unwrap_or(0);
    // What max_cost_usd is actually judged against - own spend plus
    // whatever was carried in. This is what `cost_usd` has always reported;
    // `budget_position_usd` is the same number under the name that says so.
    let budget_position_usd = m
        .get("cost_usd")
        .or_else(|| s.get("cost_usd"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    // Never negative and never larger than the run's own total: a hand-edited
    // meta.json must not be able to turn a fleet total into a refund.
    let carried_cost_usd = m
        .get("carried_budget")
        .and_then(|c| c.get("cost_usd"))
        .and_then(Value::as_f64)
        .filter(|c| c.is_finite() && *c > 0.0)
        .unwrap_or(0.0)
        .min(budget_position_usd.max(0.0));
    // What this run itself spent, carried inheritance backed out. The clamp
    // above guarantees this is never negative for a non-negative
    // budget_position_usd.
    let own_cost_usd = budget_position_usd - carried_cost_usd;
    // Resolvable exactly when every ancestor this run's own carried_cost_usd
    // depends on can still be independently verified; see the doc comment on
    // `RunSummary::lineage_cost_usd`.
    let lineage_cost_usd = lineage_is_resolvable(dir).then_some(budget_position_usd);
    RunSummary {
        run_dir: dir.display().to_string(),
        status,
        started_utc: opt_string(&m, "started_utc"),
        exit,
        ok,
        failed,
        visit,
        repeat,
        cost_usd: budget_position_usd,
        own_cost_usd,
        carried_cost_usd,
        budget_position_usd,
        lineage_cost_usd,
    }
}

fn details(dir: &Path) -> RunDetails {
    let m = meta(dir);
    let steps = step_summaries(dir);
    RunDetails {
        summary: summary(dir, &steps),
        flow: opt_string(&m, "flow"),
        sfh_version: opt_string(&m, "sfh_version"),
        tools: m.get("tools").cloned().unwrap_or(Value::Null),
        budget_landed: budget_landing(dir),
        steps,
    }
}

pub fn list(root: &Path, limit: usize, as_json: bool) -> i32 {
    let selected: Vec<RunDetails> = run_dirs(root)
        .into_iter()
        .rev()
        .take(limit)
        .map(|d| details(&d))
        .collect();
    // `f64::sum()` uses the additive identity -0.0 for an empty iterator on
    // supported Rust versions, which surfaced as the nonsensical `$-0.0000`
    // when no runs existed. Start the fold from an explicit positive zero.
    //
    // A run started with --carry-budget-from reports its ANCESTOR's spend
    // inside its own budget_position_usd, because that is the number its
    // max_cost_usd is judged against. The ancestor's row reports those same
    // dollars, so a plain sum of the rows bills the carried spend once per
    // hop - which is exactly the miscount this listing exists to prevent.
    // Sum what each SELECTED row actually spent, i.e. own_cost_usd - this is
    // named total_own_cost_usd below for exactly that reason (P1-08): it is
    // not a lineage total (an ancestor outside --limit, or already cleaned,
    // contributes nothing to it either), and it is not each row's
    // budget_position_usd summed either. `total_cost_usd` is kept, equal to
    // it, for existing JSON consumers.
    let total_own_cost_usd = selected
        .iter()
        .fold(0.0_f64, |total, run| total + run.summary.own_cost_usd);

    if as_json {
        let runs: Vec<&RunSummary> = selected.iter().map(|r| &r.summary).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "runs": runs,
                "total_cost_usd": total_own_cost_usd,
                "total_own_cost_usd": total_own_cost_usd,
            }))
            .expect("serializing run summaries cannot fail")
        );
        return 0;
    }

    if selected.is_empty() {
        println!("no runs under {}", root.display());
    } else {
        println!(
            "{:<10} {:<16} {:>4} {:>4} {:>6} {:>5} {:>6} {:>9} {:>11} {:>11} {:>12}  RUN DIR",
            "STATUS",
            "STARTED(UTC)",
            "EXIT",
            "OK",
            "FAILED",
            "VISIT",
            "REPEAT",
            "OWN_USD",
            "CARRIED_USD",
            "BUDGET_USD",
            "LINEAGE_USD"
        );
        for run in &selected {
            let r = &run.summary;
            // A dash, not a number, when the ancestry cannot be verified -
            // never a value that looks like a total but silently is not one
            // (P1-08).
            let lineage = r
                .lineage_cost_usd
                .map(|v| format!("{v:.4}"))
                .unwrap_or_else(|| "-".to_string());
            println!(
                "{:<10} {:<16} {:>4} {:>4} {:>6} {:>5} {:>6} {:>9.4} {:>11.4} {:>11.4} {:>12}  {}",
                r.status.as_deref().unwrap_or("-"),
                r.started_utc.as_deref().unwrap_or("-"),
                r.exit
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                r.ok,
                r.failed,
                r.visit,
                r.repeat,
                r.own_cost_usd,
                r.carried_cost_usd,
                r.budget_position_usd,
                lineage,
                r.run_dir
            );
        }
    }
    println!("\ntotal own cost across the {} run(s) listed: ${total_own_cost_usd:.4} (excludes carried spend, and excludes any ancestor outside this listing)", selected.len());
    0
}

pub fn show(dir: &Path, as_json: bool) -> i32 {
    match contain::read_contained_opt(dir, "log.jsonl") {
        Ok(Some(_)) => {}
        Ok(None) => {
            return bare_json_error(
                as_json,
                dir,
                &format!("{} is not an sfh run directory", dir.display()),
            );
        }
        Err(e) => {
            return bare_json_error(
                as_json,
                dir,
                &format!("refusing unsafe run directory {}: {e}", dir.display()),
            );
        }
    }
    let run = details(dir);
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&run).expect("serializing run details cannot fail")
        );
        return 0;
    }

    let m = meta(dir);
    println!("run dir : {}", dir.display());
    println!("flow    : {}", get(&m, "flow"));
    println!("sfh     : {}", get(&m, "sfh_version"));
    println!("started : {}", get(&m, "started_utc"));
    println!("status  : {}", run.summary.status.as_deref().unwrap_or("-"));
    println!(
        "exit    : {}",
        run.summary
            .exit
            .map(|x| x.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    // Said out loud, because otherwise the total at the bottom looks like money
    // this run spent and part of it belongs to an earlier one.
    if run.summary.carried_cost_usd > 0.0 {
        println!(
            "carried : ${:.4} of the total was inherited from {}",
            run.summary.carried_cost_usd,
            m.get("carried_budget")
                .map(|c| get(c, "from"))
                .unwrap_or("-")
        );
    }
    if let Some(b) = &run.budget_landed {
        println!(
            "budget  : landed on {} after ${:.4} / {}s -> goto {}",
            b.trigger, b.spent_usd, b.elapsed_sec, b.goto
        );
    }
    if let Some(t) = m.get("tools").and_then(Value::as_object) {
        for (k, v) in t {
            // A tool with several distinct bins records an array of entries.
            match v.as_array() {
                Some(entries) => {
                    for e in entries {
                        println!("tool    : {k} = {} ({})", get(e, "version"), get(e, "bin"));
                    }
                }
                None => println!("tool    : {k} = {} ({})", get(v, "version"), get(v, "bin")),
            }
        }
    }
    println!();
    println!(
        "{:<26} {:>4} {:>4} {:>6} {:>5} {:>6} {:>8} {:>9} {:>10}",
        "STEP", "EXIT", "OK", "FAILED", "VISIT", "REPEAT", "SECS", "CHARS", "COST_USD"
    );
    for step in &run.steps {
        println!(
            "{:<26} {:>4} {:>4} {:>6} {:>5} {:>6} {:>8.1} {:>9} {:>10.4}",
            step.step,
            step.exit
                .map(|x| x.to_string())
                .unwrap_or_else(|| "-".to_string()),
            step.ok,
            step.failed,
            step.visit,
            step.repeat,
            step.dur_ms as f64 / 1000.0,
            step.output_chars,
            step.cost_usd,
        );
    }
    println!(
        "\nown cost: ${:.4}   budget position (own + carried, judged against max_cost_usd): ${:.4}",
        run.summary.own_cost_usd, run.summary.budget_position_usd
    );
    // Only worth a line when this run actually has ancestry to report on;
    // for an ordinary run lineage_cost_usd is just own_cost_usd again.
    if run.summary.carried_cost_usd > 0.0 {
        match run.summary.lineage_cost_usd {
            Some(v) => println!("lineage cost (this run's full --carry-budget-from ancestry): ${v:.4}"),
            None => println!(
                "lineage cost: not resolvable - an ancestor in the --carry-budget-from chain is missing or unreadable (see `carried` above)"
            ),
        }
    }
    0
}

/// Explain the last durable control-flow facts without re-running anything.
/// This is intentionally log-backed rather than inferred from filenames: the
/// log is the resume contract and therefore the authoritative answer to
/// "where did this run stop and what will happen next?".
pub fn why(dir: &Path, as_json: bool) -> i32 {
    let log = match contain::read_contained_opt(dir, "log.jsonl") {
        Ok(Some(log)) => log,
        Ok(None) => {
            return bare_json_error(
                as_json,
                dir,
                &format!("{} is not an sfh run directory", dir.display()),
            );
        }
        Err(e) => {
            return bare_json_error(
                as_json,
                dir,
                &format!("cannot safely read {}/log.jsonl: {e}", dir.display()),
            );
        }
    };
    let mut last = Value::Null;
    let mut position = Value::Null;
    let mut last_failed_step = Value::Null;
    let mut unfinished: BTreeMap<String, Value> = BTreeMap::new();
    let mut fanout: BTreeMap<String, Value> = BTreeMap::new();
    let mut fallbacks: BTreeMap<String, Value> = BTreeMap::new();
    let mut postprocessing: BTreeMap<String, Value> = BTreeMap::new();
    for line in log.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = event.get("event").and_then(Value::as_str).unwrap_or("");
        let step = event
            .get("step")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let visit = event.get("visit").and_then(Value::as_u64).unwrap_or(1);
        let parent = event.get("parent").and_then(Value::as_str).unwrap_or("");
        let key = format!("{parent}/{step}@{visit}");
        let stage_key = if parent.is_empty() {
            format!("{step}@{visit}")
        } else {
            format!("{parent}/{step}@{visit}")
        };
        match kind {
            "step_start" => {
                unfinished.insert(key, event.clone());
            }
            "step_end" => {
                unfinished.remove(&key);
                if event.get("exit").and_then(Value::as_i64).unwrap_or(1) != 0 {
                    last_failed_step = event.clone();
                }
                if event
                    .get("next_fallback")
                    .and_then(Value::as_str)
                    .is_some_and(|profile| !profile.is_empty())
                {
                    fallbacks.insert(stage_key.clone(), event.clone());
                } else {
                    fallbacks.remove(&stage_key);
                }
                if event.get("postprocess_pending").and_then(Value::as_bool) == Some(true) {
                    postprocessing.insert(stage_key, event.clone());
                }
            }
            "group_start" | "foreach_start" => {
                fanout.insert(format!("{step}@{visit}"), event.clone());
            }
            "aggregate_end" => {
                let stage_key = format!("{step}@{visit}");
                fanout.remove(&stage_key);
                if event.get("postprocess_pending").and_then(Value::as_bool) == Some(true) {
                    postprocessing.insert(stage_key, event.clone());
                }
            }
            "postprocess_end" => {
                postprocessing.remove(&format!("{step}@{visit}"));
            }
            "position" => position = event.clone(),
            _ => {}
        }
        last = event;
    }
    let status = status(dir);
    let state = get(&status, "state").to_string();
    let error = opt_string(&status, "error");
    let current = opt_string(&status, "current_step");
    let harness_diagnostic = (last_failed_step.get("step").and_then(Value::as_str)
        == current.as_deref())
    .then(|| {
        last_failed_step
            .get("harness_diagnostic")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
    })
    .flatten();
    // A step whose structured protocol never completed failed for a reason no
    // amount of reading the tool's own stderr explains: sfh refused to certify
    // a turn nobody proved had finished. Surfaced separately from the free-text
    // diagnostic so a machine caller can branch on it (spec 15.3). Absent from
    // logs written before v1.2, which read as `null`.
    let protocol_failure = last_failed_step
        .get("protocol_state")
        .and_then(Value::as_str)
        .filter(|s| matches!(*s, "invalid" | "missing_terminal"))
        .map(|s| {
            serde_json::json!({
                "step": last_failed_step.get("step"),
                "protocol_state": s,
                "terminal_seen": last_failed_step.get("terminal_seen"),
                "final_message_seen": last_failed_step.get("final_message_seen"),
                "malformed_records": last_failed_step.get("malformed_records"),
            })
        });
    let explanation = if let Some(error) = &error {
        match &harness_diagnostic {
            Some(diagnostic) => format!("{error}: {diagnostic}"),
            None => error.clone(),
        }
    } else if let Some(checkpoint) = fallbacks.values().next_back() {
        format!(
            "a failed attempt durably selected fallback profile '{}'; resume will continue that profile in the same visit",
            checkpoint
                .get("next_fallback")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        )
    } else if !postprocessing.is_empty() {
        "the step result is durable but compact/notes post-processing has no postprocess_end; resume will finish post-processing without rerunning the step".into()
    } else if !fanout.is_empty() {
        "a fan-out started but no aggregate_end was durably recorded; resume will restore successful member step_end records and run only the remainder".into()
    } else if !unfinished.is_empty() {
        "one or more leaves started without a durable step_end; resume will run those leaves again"
            .into()
    } else if let Some(next) = position.get("next").and_then(Value::as_str) {
        format!("the last durable routing decision selected '{next}'")
    } else if state == "done" {
        "the run completed successfully".into()
    } else {
        "the log has no later durable control-flow decision".into()
    };
    let report = serde_json::json!({
        "run_dir": dir.display().to_string(),
        "state": state,
        "current_step": current,
        "error": error,
        "harness_diagnostic": harness_diagnostic,
        "protocol_failure": protocol_failure,
        "explanation": explanation,
        "last_event": last,
        "last_position": position,
        "unfinished_leaves": unfinished.into_values().collect::<Vec<_>>(),
        "unfinished_fanouts": fanout.into_values().collect::<Vec<_>>(),
        "unfinished_fallbacks": fallbacks.into_values().collect::<Vec<_>>(),
        "unfinished_postprocessing": postprocessing.into_values().collect::<Vec<_>>(),
    });
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        println!("run dir : {}", dir.display());
        println!("state   : {}", report["state"].as_str().unwrap_or("-"));
        if let Some(step) = report["current_step"].as_str().filter(|s| !s.is_empty()) {
            println!("current : {step}");
        }
        println!(
            "why     : {}",
            report["explanation"].as_str().unwrap_or("-")
        );
        if let Some(next) = report["last_position"].get("next").and_then(Value::as_str) {
            println!("next    : {next}");
        }
    }
    0
}

#[derive(Default, Debug)]
pub struct RetentionReport {
    pub removed: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

fn terminal_for_retention(dir: &Path) -> Result<bool, String> {
    let Some(text) = contain::read_contained_opt(dir, "status.json")? else {
        return Ok(false);
    };
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| format!("cannot parse {}/status.json: {error}", dir.display()))?;
    Ok(matches!(
        value.get("state").and_then(Value::as_str),
        Some("done" | "failed" | "stuck" | "stopped" | "dead")
    ))
}

fn identity_verified_for_retention(dir: &Path) -> Result<bool, String> {
    let snapshot = crate::watch::read(dir)?;
    crate::watch::nonce_consistent(dir, &snapshot)?;
    Ok(true)
}

/// A retained managed worktree needs its run manifest as the durable link back
/// to source, branch, and ownership. Absence is safe; ambiguity is not.
fn managed_workspace_is_gone(dir: &Path) -> Result<bool, String> {
    let Some(text) = contain::read_contained_opt(dir, crate::workspace::MANIFEST)? else {
        return Ok(true);
    };
    let value: Value = serde_json::from_str(&text).map_err(|error| {
        format!(
            "cannot parse {}/{}: {error}",
            dir.display(),
            crate::workspace::MANIFEST
        )
    })?;
    let workspace = crate::workspace::Workspace::from_manifest(&value).ok_or_else(|| {
        format!(
            "cannot verify managed workspace in {}/{}",
            dir.display(),
            crate::workspace::MANIFEST
        )
    })?;
    if workspace.mode != crate::flow::WorkspaceMode::GitWorktree {
        return Ok(true);
    }
    match workspace.path.symlink_metadata() {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(format!(
            "cannot inspect managed workspace {}: {error}",
            workspace.path.display()
        )),
    }
}

/// Opportunistically prune old run evidence under a host-owned policy.
/// Every uncertain fact keeps the directory; cleanup must never be the event
/// that decides whether a run or its managed worktree was still live.
pub fn apply_retention(root: &Path, policy: crate::state::RunRetention) -> RetentionReport {
    let mut report = RetentionReport::default();
    let dirs = run_dirs(root);
    if dirs.len() <= policy.keep {
        return report;
    }
    let canon_root = match root.canonicalize() {
        Ok(root) => root,
        Err(error) => {
            report.warnings.push(format!(
                "cannot resolve runs root {}: {error}",
                root.display()
            ));
            return report;
        }
    };
    let now = std::time::SystemTime::now();
    let cutoff = std::time::Duration::from_secs(policy.older_than_days.saturating_mul(86_400));

    for dir in dirs.iter().take(dirs.len() - policy.keep) {
        let old_enough = std::fs::metadata(dir)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > cutoff);
        if !old_enough {
            continue;
        }
        if !matches!(terminal_for_retention(dir), Ok(true))
            || !matches!(identity_verified_for_retention(dir), Ok(true))
            || !matches!(crate::watch::owner_verifiably_dead(dir), Ok(Some(true)))
            || !matches!(managed_workspace_is_gone(dir), Ok(true))
        {
            continue;
        }

        // The lease closes the check/delete race with resume or an explicitly
        // targeted second run. Re-check all mutable facts after claiming it.
        let lease = match contain::try_run_lease_for_delete(dir) {
            Ok(lease) => lease,
            Err(contain::RunLeaseError::Busy) => continue,
            Err(contain::RunLeaseError::Io(error)) => {
                report.warnings.push(format!(
                    "cannot lock retention candidate {}: {error}",
                    dir.display()
                ));
                continue;
            }
        };
        let still_safe = dir
            .symlink_metadata()
            .map(|metadata| metadata.file_type().is_dir())
            .unwrap_or(false)
            && dir
                .canonicalize()
                .map(|resolved| resolved.starts_with(&canon_root))
                .unwrap_or(false)
            && matches!(terminal_for_retention(dir), Ok(true))
            && matches!(identity_verified_for_retention(dir), Ok(true))
            && matches!(crate::watch::owner_verifiably_dead(dir), Ok(Some(true)))
            && matches!(managed_workspace_is_gone(dir), Ok(true));
        if !still_safe {
            drop(lease);
            continue;
        }
        match std::fs::remove_dir_all(dir) {
            Ok(()) => report.removed.push(dir.clone()),
            Err(error) => report.warnings.push(format!(
                "cannot remove retained run {}: {error}",
                dir.display()
            )),
        }
        drop(lease);
    }
    report
}

/// Delete run dirs older than `days`, always keeping the newest `keep`.
pub fn clean(root: &Path, days: u64, keep: usize, dry: bool) -> i32 {
    let dirs = run_dirs(root);
    if dirs.len() <= keep {
        println!("nothing to clean ({} runs, keeping {keep})", dirs.len());
        return 0;
    }
    let now = std::time::SystemTime::now();
    let cutoff = std::time::Duration::from_secs(days.saturating_mul(86_400));
    let canon_root = match root.canonicalize() {
        Ok(root) => root,
        Err(e) => {
            eprintln!("sfh: cannot resolve runs root {}: {e}", root.display());
            return 2;
        }
    };
    let mut removed = 0;
    for d in dirs.iter().take(dirs.len() - keep) {
        // Re-check immediately before a destructive operation. The runs root
        // is expected to be private, but a path swapped for a symlink/junction
        // after enumeration must still be refused.
        let still_safe = d
            .symlink_metadata()
            .map(|m| m.file_type().is_dir())
            .unwrap_or(false)
            && d.canonicalize()
                .map(|resolved| resolved.starts_with(&canon_root))
                .unwrap_or(false);
        if !still_safe {
            eprintln!("sfh: refusing unsafe run directory {}", d.display());
            continue;
        }
        let age_ok = std::fs::metadata(d)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .map(|age| age > cutoff)
            .unwrap_or(false);
        if !age_ok {
            continue;
        }
        if dry {
            println!("would remove {}", d.display());
        } else if let Err(e) = std::fs::remove_dir_all(d) {
            eprintln!("sfh: cannot remove {}: {e}", d.display());
            continue;
        } else {
            println!("removed {}", d.display());
        }
        removed += 1;
    }
    println!(
        "{} {removed} run dir(s) older than {days}d (kept newest {keep})",
        if dry { "would remove" } else { "removed" }
    );
    0
}

// ---------------------------------------------------------------------------
// `sfh workspaces` - the managed working environments runs left behind.
//
// Every operation here is bounded by one rule: sfh removes only a path it can
// prove it created, and it never discards uncommitted work without being told
// to in so many words. Both checks are made immediately before the deletion,
// not once at the start, because between a decision and its execution a path
// can be swapped for something else entirely.
// ---------------------------------------------------------------------------

/// The workspace a run recorded, if any.
fn workspace_of(dir: &Path) -> Option<crate::workspace::Workspace> {
    let v = read_json(dir, crate::workspace::MANIFEST);
    crate::workspace::Workspace::from_manifest(&v)
}

/// What a workspace looks like right now, as opposed to when it was recorded.
fn workspace_row(dir: &Path) -> Option<Value> {
    let ws = workspace_of(dir)?;
    let st = status(dir);
    let state = get(&st, "state").to_string();
    let exists = ws.path.is_dir();
    let dirty = exists
        .then(|| crate::workspace::is_dirty(&ws.path).ok())
        .flatten();
    let owned = crate::workspace::verify_ownership(&ws);
    Some(serde_json::json!({
        "run_dir": dir.display().to_string(),
        "run_state": state,
        "workspace_id": ws.id,
        "mode": ws.mode.as_str(),
        "path": ws.path.display().to_string(),
        "branch": ws.branch,
        "base_commit": ws.base_commit,
        "source_root": ws.source_root.display().to_string(),
        "exists": exists,
        "dirty": dirty,
        "sfh_owned": owned.is_ok(),
        "ownership": owned.err(),
        "cleanup": ws.cleanup.as_str(),
    }))
}

fn emit_ws(command: &str, ok: bool, body: Value, as_json: bool, human: impl FnOnce()) -> i32 {
    if as_json {
        crate::machine::emit(&crate::machine::envelope(
            command,
            ok,
            if ok { 0 } else { 1 },
            body,
        ));
    } else {
        human();
    }
    if ok {
        0
    } else {
        1
    }
}

pub fn workspaces_list(root: &Path, as_json: bool) -> i32 {
    let rows: Vec<Value> = run_dirs(root)
        .into_iter()
        .rev()
        .filter_map(|d| workspace_row(&d))
        .collect();
    let printable = rows.clone();
    emit_ws(
        "workspaces list",
        true,
        serde_json::json!({"workspaces": rows, "runs_dir": root.display().to_string()}),
        as_json,
        || {
            if printable.is_empty() {
                println!("no managed workspaces recorded under {}", root.display());
                return;
            }
            for w in &printable {
                println!(
                    "{}  {}  {}  {}{}",
                    get(w, "run_state"),
                    get(w, "mode"),
                    get(w, "path"),
                    if w["exists"].as_bool() == Some(true) {
                        ""
                    } else {
                        "(gone) "
                    },
                    match w["dirty"].as_bool() {
                        Some(true) => "DIRTY",
                        Some(false) => "clean",
                        None => "dirty:unknown",
                    }
                );
            }
        },
    )
}

pub fn workspaces_show(dir: &Path, as_json: bool) -> i32 {
    match workspace_row(dir) {
        Some(row) => {
            let printable = row.clone();
            emit_ws("workspaces show", true, row, as_json, || {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&printable).unwrap_or_default()
                );
            })
        }
        None => {
            if as_json {
                crate::machine::emit(&crate::machine::error_envelope(
                    "workspaces show",
                    crate::machine::ErrorCode::WorkspaceMissing,
                    "this run recorded no managed workspace",
                    1,
                    serde_json::json!({"run_dir": dir.display().to_string()}),
                ));
            } else {
                eprintln!("sfh: {} recorded no managed workspace", dir.display());
            }
            1
        }
    }
}

/// Remove the managed workspaces of runs that are finished and clean.
///
/// A workspace is a candidate only when its run reached a terminal state, its
/// worktree has nothing uncommitted, and sfh owns it. Anything else is listed
/// with the reason it was skipped - a `clean` that silently did nothing is
/// indistinguishable from one that silently did too much.
pub fn workspaces_clean(
    root: &Path,
    older_than_days: Option<u64>,
    dry_run: bool,
    as_json: bool,
) -> i32 {
    let cutoff = older_than_days.map(|d| {
        std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(d * 86_400))
            .unwrap_or(std::time::UNIX_EPOCH)
    });
    let mut removed = Vec::new();
    let mut skipped = Vec::new();
    for dir in run_dirs(root) {
        let Some(ws) = workspace_of(&dir) else {
            continue;
        };
        let path = ws.path.display().to_string();
        if let Some(cutoff) = cutoff {
            let modified = dir.metadata().and_then(|m| m.modified()).ok();
            if modified.map(|m| m > cutoff).unwrap_or(true) {
                skipped
                    .push(serde_json::json!({"path": path, "reason": "newer than --older-than"}));
                continue;
            }
        }
        let state = get(&status(&dir), "state").to_string();
        if !matches!(
            state.as_str(),
            "done" | "failed" | "stuck" | "stopped" | "dead"
        ) {
            skipped.push(serde_json::json!({"path": path, "reason": format!("the run is '{state}', not finished")}));
            continue;
        }
        if state != "done" {
            skipped.push(serde_json::json!({
                "path": path,
                "reason": format!("the run ended as '{state}'; its workspace holds the evidence and is kept")
            }));
            continue;
        }
        if !ws.path.is_dir() {
            skipped.push(serde_json::json!({"path": path, "reason": "already gone"}));
            continue;
        }
        if let Err(why) = crate::workspace::verify_ownership(&ws) {
            skipped.push(serde_json::json!({"path": path, "reason": why}));
            continue;
        }
        match crate::workspace::is_dirty(&ws.path) {
            Ok(true) => {
                skipped.push(serde_json::json!({
                    "path": path,
                    "reason": "uncommitted changes; use `sfh workspaces remove <run-dir> --discard` to drop them deliberately"
                }));
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                skipped.push(serde_json::json!({"path": path, "reason": format!("cannot tell whether it is dirty: {e}")}));
                continue;
            }
        }
        if dry_run {
            removed.push(serde_json::json!({"path": path, "action": "would remove"}));
            continue;
        }
        match crate::workspace::remove_worktree(&ws) {
            Ok(()) => removed.push(serde_json::json!({"path": path, "action": "removed"})),
            Err(e) => skipped.push(serde_json::json!({"path": path, "reason": e})),
        }
    }
    let (r, s) = (removed.clone(), skipped.clone());
    emit_ws(
        "workspaces clean",
        true,
        serde_json::json!({"removed": removed, "skipped": skipped, "dry_run": dry_run}),
        as_json,
        || {
            for x in &r {
                println!("{}: {}", get(x, "action"), get(x, "path"));
            }
            for x in &s {
                println!("kept {}: {}", get(x, "path"), get(x, "reason"));
            }
            if r.is_empty() && s.is_empty() {
                println!("no managed workspaces to clean");
            }
            println!("(the branch each workspace created is always kept)");
        },
    )
}

/// Remove one run's managed workspace. `--discard` is the only way sfh drops
/// uncommitted work, and it still refuses a path it cannot prove it created.
pub fn workspaces_remove(dir: &Path, discard: bool, as_json: bool) -> i32 {
    let Some(ws) = workspace_of(dir) else {
        if as_json {
            crate::machine::emit(&crate::machine::error_envelope(
                "workspaces remove",
                crate::machine::ErrorCode::WorkspaceMissing,
                "this run recorded no managed workspace",
                1,
                serde_json::json!({"run_dir": dir.display().to_string()}),
            ));
        } else {
            eprintln!("sfh: {} recorded no managed workspace", dir.display());
        }
        return 1;
    };
    let fail = |code: crate::machine::ErrorCode, msg: String| -> i32 {
        if as_json {
            crate::machine::emit(&crate::machine::error_envelope(
                "workspaces remove",
                code,
                &msg,
                1,
                serde_json::json!({"run_dir": dir.display().to_string()}),
            ));
        } else {
            eprintln!("sfh: {msg}");
        }
        1
    };
    if let Err(why) = crate::workspace::verify_ownership(&ws) {
        return fail(
            crate::machine::ErrorCode::WorkspaceUnowned,
            format!("refusing to remove {}: {why}", ws.path.display()),
        );
    }
    if !discard {
        match crate::workspace::is_dirty(&ws.path) {
            Ok(true) => {
                return fail(
                    crate::machine::ErrorCode::WorkspaceDrift,
                    format!(
                        "{} has uncommitted changes. Commit them, or pass --discard to drop them.",
                        ws.path.display()
                    ),
                )
            }
            Ok(false) => {}
            Err(e) => {
                return fail(
                    crate::machine::ErrorCode::WorkspaceDrift,
                    format!("cannot tell whether {} is dirty: {e}", ws.path.display()),
                )
            }
        }
    }
    match crate::workspace::remove_worktree(&ws) {
        Ok(()) => {
            let path = ws.path.display().to_string();
            emit_ws(
                "workspaces remove",
                true,
                serde_json::json!({"removed": path, "branch_kept": ws.branch}),
                as_json,
                || {
                    println!("removed {path}");
                    if let Some(b) = &ws.branch {
                        println!("the branch {b} is kept");
                    }
                },
            )
        }
        Err(e) => fail(crate::machine::ErrorCode::WorkspaceUnowned, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retention_run(root: &Path, name: &str, owner_start: Option<u64>) -> PathBuf {
        let dir = root.join(name);
        contain::mkdir_private(&dir).unwrap();
        std::fs::write(dir.join("log.jsonl"), "{\"event\":\"run_end\"}\n").unwrap();
        contain::write_private_atomic(
            &dir.join("status.json"),
            serde_json::json!({
                "state": "done",
                "pid": std::process::id(),
                "pid_start": owner_start,
                "nonce": "retention-test",
            })
            .to_string(),
        )
        .unwrap();
        contain::write_nonce(&dir, std::process::id(), owner_start, "retention-test").unwrap();
        dir
    }

    #[test]
    fn automatic_retention_never_removes_a_live_run_or_a_remaining_worktree() {
        let root =
            std::env::temp_dir().join(format!("sfh-retention-runs-{}", contain::random_nonce()));
        contain::mkdir_private(&root).unwrap();

        let removable = retention_run(&root, "001-removable", Some(1));
        let live = retention_run(
            &root,
            "002-live",
            crate::execute::pid_start_time(std::process::id()),
        );
        let with_workspace = retention_run(&root, "003-workspace", Some(1));
        let workspace_path = root.join("managed-worktree");
        contain::mkdir_private(&workspace_path).unwrap();
        let workspace = crate::workspace::Workspace {
            id: "primary".into(),
            mode: crate::flow::WorkspaceMode::GitWorktree,
            source_root: root.clone(),
            path: workspace_path.clone(),
            base_ref: Some("main".into()),
            base_commit: Some("abc".into()),
            branch: Some("sfh/test".into()),
            created_by_sfh: true,
            ownership_nonce: Some("workspace-test".into()),
            cleanup: crate::flow::WorkspaceCleanup::Keep,
        };
        contain::write_private_atomic(
            &with_workspace.join(crate::workspace::MANIFEST),
            workspace.to_json(None).to_string(),
        )
        .unwrap();
        let newest = retention_run(&root, "004-newest", Some(1));
        std::thread::sleep(std::time::Duration::from_millis(20));

        let report = apply_retention(
            &root,
            crate::state::RunRetention {
                older_than_days: 0,
                keep: 1,
            },
        );
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert!(
            !removable.exists(),
            "a proven-dead terminal run is eligible"
        );
        assert!(
            live.exists(),
            "a live owner must always win over status.json"
        );
        assert!(
            with_workspace.exists(),
            "the manifest for an existing managed worktree must be kept"
        );
        assert!(
            workspace_path.exists(),
            "retention never deletes the worktree"
        );
        assert!(newest.exists(), "the newest keep entries are unconditional");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn consecutive_hashes_count_repeats_after_the_first() {
        let mut step = StepAccumulator::default();
        for (visit, hash) in [(1, "a"), (2, "a"), (3, "a"), (4, "b"), (5, "b")] {
            step.record(&serde_json::json!({
                "exit": 0,
                "visit": visit,
                "output_hash": hash,
            }));
        }
        assert_eq!(step.visit, 5);
        assert_eq!(step.repeat, 2);
        assert_eq!(step.ok, 5);
        assert_eq!(step.failed, 0);
    }

    #[test]
    fn a_changed_or_missing_hash_breaks_the_repeat_streak() {
        let mut step = StepAccumulator::default();
        for event in [
            serde_json::json!({"exit": 1, "visit": 1, "output_hash": "a"}),
            serde_json::json!({"exit": 0, "visit": 2}),
            serde_json::json!({"exit": 0, "visit": 3, "output_hash": "a"}),
            serde_json::json!({"exit": 0, "visit": 4, "output_hash": "a"}),
        ] {
            step.record(&event);
        }
        assert_eq!(step.repeat, 1);
        assert_eq!(step.ok, 3);
        assert_eq!(step.failed, 1);
    }

    // ---- P1-08: a run's cost fields must be individually correct, and a
    // lineage total must be absent rather than wrong when an ancestor is
    // gone ----

    fn cost_fields_test_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sfh-runs-cost-{tag}-{}", contain::random_nonce()));
        contain::mkdir_private(&dir).unwrap();
        dir
    }

    #[test]
    fn own_carried_and_budget_position_cost_fields_are_individually_correct() {
        let base = cost_fields_test_dir("fields");

        let ancestor = base.join("ancestor");
        contain::mkdir_private(&ancestor).unwrap();
        std::fs::write(ancestor.join("log.jsonl"), "{\"event\":\"run_start\"}\n").unwrap();
        contain::write_private_atomic(
            &ancestor.join("meta.json"),
            serde_json::json!({"cost_usd": 2.0}).to_string(),
        )
        .unwrap();

        let child = base.join("child");
        contain::mkdir_private(&child).unwrap();
        std::fs::write(child.join("log.jsonl"), "{\"event\":\"run_start\"}\n").unwrap();
        contain::write_private_atomic(
            &child.join("meta.json"),
            serde_json::json!({
                "cost_usd": 5.0,
                "carried_budget": {"cost_usd": 2.0, "from": ancestor.display().to_string()},
            })
            .to_string(),
        )
        .unwrap();

        let s = summary(&child, &[]);
        assert_eq!(
            s.budget_position_usd, 5.0,
            "own + carried - what max_cost_usd is judged against"
        );
        assert_eq!(s.carried_cost_usd, 2.0, "what this run inherited");
        assert_eq!(
            s.own_cost_usd, 3.0,
            "what this run itself spent: budget position minus carried"
        );
        assert_eq!(
            s.cost_usd, 5.0,
            "cost_usd is kept for existing consumers, identical to budget_position_usd"
        );
        assert_eq!(
            s.lineage_cost_usd,
            Some(5.0),
            "the ancestor is present and readable, so the lineage total is resolvable"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn lineage_cost_usd_is_absent_rather_than_wrong_when_an_ancestor_is_gone() {
        let base = cost_fields_test_dir("gone-ancestor");
        // Names an ancestor that was never created here, simulating `runs
        // clean` having removed it after the carry that recorded it.
        let cleaned_ancestor = base.join("cleaned-ancestor");

        let child = base.join("child");
        contain::mkdir_private(&child).unwrap();
        std::fs::write(child.join("log.jsonl"), "{\"event\":\"run_start\"}\n").unwrap();
        contain::write_private_atomic(
            &child.join("meta.json"),
            serde_json::json!({
                "cost_usd": 5.0,
                "carried_budget": {"cost_usd": 2.0, "from": cleaned_ancestor.display().to_string()},
            })
            .to_string(),
        )
        .unwrap();

        let s = summary(&child, &[]);
        // The clamp still protects these three: they are durable in THIS
        // run's own meta.json and do not need the ancestor to still exist.
        assert_eq!(s.budget_position_usd, 5.0);
        assert_eq!(s.carried_cost_usd, 2.0);
        assert_eq!(s.own_cost_usd, 3.0);
        // But the lineage total cannot be independently re-verified, so it
        // must be absent rather than a value nothing backs (P1-08) - never
        // silently substituted with budget_position_usd or a partial sum.
        assert_eq!(s.lineage_cost_usd, None);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_hand_edited_carried_cost_cannot_exceed_the_runs_own_total_or_go_negative() {
        let dir = cost_fields_test_dir("clamp");
        std::fs::write(dir.join("log.jsonl"), "{\"event\":\"run_start\"}\n").unwrap();
        contain::write_private_atomic(
            &dir.join("meta.json"),
            serde_json::json!({
                "cost_usd": 3.0,
                // Hand-edited to claim MORE was carried than the run's own
                // total: must clamp to 3.0, never turn into a refund via a
                // negative own_cost_usd.
                "carried_budget": {"cost_usd": 999.0, "from": "/nowhere"},
            })
            .to_string(),
        )
        .unwrap();
        let s = summary(&dir, &[]);
        assert_eq!(s.carried_cost_usd, 3.0);
        assert_eq!(s.own_cost_usd, 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lineage_is_resolvable_refuses_a_cycle_instead_of_looping_forever() {
        let base = cost_fields_test_dir("cycle");
        let a = base.join("a");
        let b = base.join("b");
        contain::mkdir_private(&a).unwrap();
        contain::mkdir_private(&b).unwrap();
        contain::write_private_atomic(
            &a.join("meta.json"),
            serde_json::json!({"carried_budget": {"from": b.display().to_string()}}).to_string(),
        )
        .unwrap();
        contain::write_private_atomic(
            &b.join("meta.json"),
            serde_json::json!({"carried_budget": {"from": a.display().to_string()}}).to_string(),
        )
        .unwrap();
        assert!(!lineage_is_resolvable(&a));
        let _ = std::fs::remove_dir_all(&base);
    }
}
