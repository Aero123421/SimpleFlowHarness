//! `sfh runs` - browse and prune the evidence trail.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn run_dirs(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = match std::fs::read_dir(root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.join("log.jsonl").exists())
            .collect(),
        Err(_) => Vec::new(),
    };
    v.sort();
    v
}

fn read_json(dir: &Path, name: &str) -> Value {
    std::fs::read_to_string(dir.join(name))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
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
}

#[derive(Serialize)]
struct RunDetails {
    #[serde(flatten)]
    summary: RunSummary,
    flow: Option<String>,
    sfh_version: Option<String>,
    tools: Value,
    steps: Vec<StepSummary>,
}

fn step_summaries(dir: &Path) -> Vec<StepSummary> {
    let log = std::fs::read_to_string(dir.join("log.jsonl")).unwrap_or_default();
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
    let status = opt_string(&m, "status").or_else(|| opt_string(&s, "state"));
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
    let total_cost_usd: f64 = selected.iter().map(|r| r.summary.cost_usd).sum();

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
        eprintln!("no runs under {}", root.display());
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
    if !dir.join("log.jsonl").exists() {
        eprintln!("sfh: {} is not an sfh run directory", dir.display());
        return 2;
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

/// Delete run dirs older than `days`, always keeping the newest `keep`.
pub fn clean(root: &Path, days: u64, keep: usize, dry: bool) -> i32 {
    let dirs = run_dirs(root);
    if dirs.len() <= keep {
        eprintln!("nothing to clean ({} runs, keeping {keep})", dirs.len());
        return 0;
    }
    let now = std::time::SystemTime::now();
    let cutoff = std::time::Duration::from_secs(days * 86400);
    let mut removed = 0;
    for d in dirs.iter().take(dirs.len() - keep) {
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
    eprintln!(
        "{} {removed} run dir(s) older than {days}d (kept newest {keep})",
        if dry { "would remove" } else { "removed" }
    );
    0
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
