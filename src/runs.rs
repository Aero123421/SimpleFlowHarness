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
    cost_usd: f64,
    /// How much of `cost_usd` this run INHERITED from an earlier one via
    /// `--carry-budget-from` rather than spending itself. `cost_usd` is the
    /// run's position against `max_cost_usd`, so it has to include the carried
    /// spend - but the ancestor's own row already reports those same dollars,
    /// so anything summing rows must take this back out or it counts the
    /// carried spend once per hop. Zero for the ordinary run that carried
    /// nothing.
    carried_cost_usd: f64,
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
    let cost_usd = m
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
        .min(cost_usd.max(0.0));
    RunSummary {
        run_dir: dir.display().to_string(),
        status,
        started_utc: opt_string(&m, "started_utc"),
        exit,
        ok,
        failed,
        visit,
        repeat,
        cost_usd,
        carried_cost_usd,
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
    // inside its own cost_usd, because that is the number its max_cost_usd is
    // judged against. The ancestor's row reports those same dollars, so a
    // plain sum of the rows bills the carried spend once per hop - which is
    // exactly the miscount this listing exists to prevent. Sum what each run
    // actually spent.
    let total_cost_usd = selected.iter().fold(0.0_f64, |total, run| {
        total + (run.summary.cost_usd - run.summary.carried_cost_usd)
    });

    if as_json {
        let runs: Vec<&RunSummary> = selected.iter().map(|r| &r.summary).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "runs": runs,
                "total_cost_usd": total_cost_usd,
            }))
            .expect("serializing run summaries cannot fail")
        );
        return 0;
    }

    if selected.is_empty() {
        println!("no runs under {}", root.display());
    } else {
        println!(
            "{:<10} {:<16} {:>4} {:>4} {:>6} {:>5} {:>6} {:>10}  RUN DIR",
            "STATUS", "STARTED(UTC)", "EXIT", "OK", "FAILED", "VISIT", "REPEAT", "COST_USD"
        );
        for run in &selected {
            let r = &run.summary;
            println!(
                "{:<10} {:<16} {:>4} {:>4} {:>6} {:>5} {:>6} {:>10.4}  {}",
                r.status.as_deref().unwrap_or("-"),
                r.started_utc.as_deref().unwrap_or("-"),
                r.exit
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                r.ok,
                r.failed,
                r.visit,
                r.repeat,
                r.cost_usd,
                r.run_dir
            );
        }
    }
    println!("\ntotal cost: ${total_cost_usd:.4}");
    0
}

pub fn show(dir: &Path, as_json: bool) -> i32 {
    match contain::read_contained_opt(dir, "log.jsonl") {
        Ok(Some(_)) => {}
        Ok(None) => {
            eprintln!("sfh: {} is not an sfh run directory", dir.display());
            return 2;
        }
        Err(e) => {
            eprintln!("sfh: refusing unsafe run directory {}: {e}", dir.display());
            return 2;
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
    println!("\ntotal cost: ${:.4}", run.summary.cost_usd);
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
            eprintln!("sfh: {} is not an sfh run directory", dir.display());
            return 2;
        }
        Err(e) => {
            eprintln!("sfh: cannot safely read {}/log.jsonl: {e}", dir.display());
            return 2;
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
}
