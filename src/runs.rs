//! `sfh runs` - browse and prune the evidence trail.

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

fn meta(dir: &Path) -> serde_json::Value {
    std::fs::read_to_string(dir.join("meta.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(serde_json::Value::Null)
}

fn status(dir: &Path) -> serde_json::Value {
    std::fs::read_to_string(dir.join("status.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(serde_json::Value::Null)
}

fn get<'a>(v: &'a serde_json::Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("-")
}

pub fn list(root: &Path, limit: usize) -> i32 {
    let dirs = run_dirs(root);
    if dirs.is_empty() {
        eprintln!("no runs under {}", root.display());
        return 0;
    }
    println!(
        "{:<10} {:<16} {:>6} {:>10}  RUN DIR",
        "STATUS", "STARTED(UTC)", "STEPS", "COST_USD"
    );
    for d in dirs.iter().rev().take(limit) {
        let m = meta(d);
        let s = status(d);
        let st = if m.get("status").is_some() {
            get(&m, "status").to_string()
        } else {
            get(&s, "state").to_string()
        };
        let steps = m
            .get("leaf_runs")
            .or_else(|| s.get("steps_done"))
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        let cost = m
            .get("cost_usd")
            .or_else(|| s.get("cost_usd"))
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0);
        println!(
            "{:<10} {:<16} {:>6} {:>10.4}  {}",
            st,
            get(&m, "started_utc"),
            steps,
            cost,
            d.display()
        );
    }
    0
}

pub fn show(dir: &Path) -> i32 {
    if !dir.join("log.jsonl").exists() {
        eprintln!("sfh: {} is not an sfh run directory", dir.display());
        return 2;
    }
    let m = meta(dir);
    println!("run dir : {}", dir.display());
    println!("flow    : {}", get(&m, "flow"));
    println!("sfh     : {}", get(&m, "sfh_version"));
    println!("started : {}", get(&m, "started_utc"));
    println!("status  : {}", get(&m, "status"));
    if let Some(t) = m.get("tools").and_then(|x| x.as_object()) {
        for (k, v) in t {
            println!("tool    : {k} = {} ({})", get(v, "version"), get(v, "bin"));
        }
    }
    println!();
    println!(
        "{:<26} {:>5} {:>8} {:>9} {:>10}",
        "STEP", "EXIT", "SECS", "CHARS", "COST_USD"
    );
    let log = std::fs::read_to_string(dir.join("log.jsonl")).unwrap_or_default();
    for line in log.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let ev = v.get("event").and_then(|x| x.as_str()).unwrap_or("");
        if ev != "step_end" {
            continue;
        }
        println!(
            "{:<26} {:>5} {:>8.1} {:>9} {:>10.4}",
            get(&v, "step"),
            v.get("exit").and_then(|x| x.as_i64()).unwrap_or(-1),
            v.get("dur_ms").and_then(|x| x.as_f64()).unwrap_or(0.0) / 1000.0,
            v.get("output_chars").and_then(|x| x.as_u64()).unwrap_or(0),
            v.get("cost_usd").and_then(|x| x.as_f64()).unwrap_or(0.0),
        );
    }
    let total = m.get("cost_usd").and_then(|x| x.as_f64()).unwrap_or(0.0);
    println!("\ntotal cost: ${total:.4}");
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
