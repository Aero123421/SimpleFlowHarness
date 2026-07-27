use serde::Deserialize;
use serde_yaml_ng as yaml;
use std::collections::{BTreeMap, HashSet};
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
    pub retry: Option<Retry>,
    /// transient (default) | any | never
    pub retry_on: Option<String>,
    /// Env applied to every child process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
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
    /// read | write | full (default: write)
    pub access: Option<String>,
    /// Agent name (opencode/claude/grok/agy --agent).
    pub agent: Option<String>,
    /// Extra raw args appended to the preset command line.
    #[serde(default)]
    pub args: Vec<String>,
    /// Custom command instead of a preset. Array = spawned directly (no shell),
    /// String = run through cmd /C (Windows) or sh -c (Unix).
    pub cmd: Option<Cmd>,
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
    pub retry: Option<Retry>,
    pub retry_on: Option<String>,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub when_contains: Option<String>,
    pub when_matches: Option<String>,
    /// Same as when_contains but only the LAST non-empty line is searched -
    /// the deterministic way to read a "VERDICT: OK" trailer.
    pub when_last_line_contains: Option<String>,
    pub when_last_line_matches: Option<String>,
    /// Step id, or "end" (finish flow, success) or "fail" (finish flow, failure).
    pub goto: String,
}

pub const TOOLS: [&str; 6] = ["codex", "claude", "opencode", "grok", "agy", "pi"];

pub fn load(path: &Path) -> Result<Flow, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut flow: Flow = yaml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    merge_global_profiles(&mut flow);
    validate(&flow)?;
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

    /// Every preset tool this flow could invoke (for version stamping).
    pub fn tools_used(&self) -> HashSet<String> {
        let mut out = HashSet::new();
        for s in &self.steps {
            out.extend(self.step_tool(s));
            for f in &s.fallback {
                out.extend(self.profiles.get(f).and_then(|p| p.tool.clone()));
            }
            if let Some(children) = &s.parallel {
                for c in children {
                    out.extend(self.step_tool(c));
                }
            }
            if let Some(c) = &s.compact {
                out.extend(c.tool.clone().or_else(|| {
                    c.use_
                        .as_ref()
                        .and_then(|u| self.profiles.get(u))
                        .and_then(|p| p.tool.clone())
                }));
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

fn validate(flow: &Flow) -> Result<(), String> {
    if flow.steps.is_empty() {
        return Err("flow has no steps".into());
    }
    let mut seen = HashSet::new();
    for s in &flow.steps {
        if !valid_id(&s.id) {
            return Err(format!(
                "step id '{}' must be non-empty and use only [A-Za-z0-9_-]",
                s.id
            ));
        }
        if !seen.insert(s.id.clone()) {
            return Err(format!("duplicate step id '{}'", s.id));
        }
        if let Some(children) = &s.parallel {
            for c in children {
                if !valid_id(&c.id) {
                    return Err(format!(
                        "step id '{}' must be non-empty and use only [A-Za-z0-9_-]",
                        c.id
                    ));
                }
                if !seen.insert(c.id.clone()) {
                    return Err(format!("duplicate step id '{}'", c.id));
                }
            }
        }
    }
    let top_ids = flow.top_ids();
    let check_goto = |ctx: &str, g: &str| -> Result<(), String> {
        if g == "end" || g == "fail" || top_ids.contains(g) {
            Ok(())
        } else {
            Err(format!(
                "{ctx}: goto target '{g}' is not a top-level step id (or end/fail)"
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
    if let Some(r) = &flow.defaults.retry_on {
        check_retry_on("defaults", r)?;
    }
    let action = |what: &str, s: &Step, v: &Option<String>, is_child: bool| -> Result<(), String> {
        let Some(oe) = v else { return Ok(()) };
        if let Some(g) = oe.strip_prefix("goto:") {
            if g == "end" || g == "fail" {
                return Ok(());
            }
            if is_child {
                return Err(format!(
                    "step '{}': {what} goto is not allowed inside parallel: (use fail or continue)",
                    s.id
                ));
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
    let check_continue_from = |s: &Step| -> Result<(), String> {
        let Some(cf) = &s.continue_from else {
            return Ok(());
        };
        if cf == &s.id {
            return Err(format!(
                "step '{}': continue_from cannot reference itself",
                s.id
            ));
        }
        let target = flow.find_step(cf).ok_or_else(|| {
            format!(
                "step '{}': continue_from target '{cf}' is not a step id",
                s.id
            )
        })?;
        if target.is_group() {
            return Err(format!(
                "step '{}': continue_from '{cf}' targets a parallel group (sessions belong to its children; target one of them)",
                s.id
            ));
        }
        if target.is_foreach() {
            return Err(format!(
                "step '{}': continue_from '{cf}' targets a foreach step (it runs many sessions; there is no single one to resume)",
                s.id
            ));
        }
        if target.cmd.is_some() {
            return Err(format!(
                "step '{}': continue_from '{cf}' targets a cmd: step, which has no session",
                s.id
            ));
        }
        Ok(())
    };
    for s in &flow.steps {
        validate_step(flow, s, false)?;
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
                validate_step(flow, c, true)?;
                action("on_error", c, &c.on_error, true)?;
                check_continue_from(c)?;
                if let Some(cf) = &c.continue_from {
                    if child_ids.contains(cf) {
                        return Err(format!(
                            "step '{}': continue_from '{cf}' targets a sibling in the same parallel group (it has not run yet)",
                            c.id
                        ));
                    }
                    if let Some(prev) = targets_seen.insert(cf, &c.id) {
                        return Err(format!(
                            "steps '{prev}' and '{}' both continue_from '{cf}' inside one parallel group - two concurrent resumes of the same session",
                            c.id
                        ));
                    }
                }
            }
        }
        for (i, r) in s.route.iter().enumerate() {
            check_goto(&format!("step '{}' route[{i}]", s.id), &r.goto)?;
            for rx in [&r.when_matches, &r.when_last_line_matches]
                .into_iter()
                .flatten()
            {
                if !rx.contains("{{") {
                    regex::Regex::new(rx)
                        .map_err(|e| format!("step '{}' route[{i}]: bad regex: {e}", s.id))?;
                }
            }
        }
    }
    Ok(())
}

fn check_retry_on(ctx: &str, v: &str) -> Result<(), String> {
    if ["transient", "any", "never"].contains(&v) {
        Ok(())
    } else {
        Err(format!("{ctx}: retry_on must be transient/any/never"))
    }
}

fn validate_step(flow: &Flow, s: &Step, is_child: bool) -> Result<(), String> {
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
            || s.use_.is_some()
            || s.model.is_some()
            || s.stdin.is_some()
            || s.agent.is_some()
            || s.compact.is_some()
            || s.bin.is_some()
            || s.effort.is_some()
            || s.access.is_some()
            || !s.args.is_empty()
            || s.cwd.is_some()
            || s.timeout_sec.is_some()
            || s.max_prompt_chars.is_some()
            || s.retry.is_some()
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
    }
    if let Some(Cmd::Shell(c)) = &s.cmd {
        if c.trim().is_empty() {
            return Err(format!("step '{sid}': cmd string is empty"));
        }
    }
    if s.cmd.is_some() && !s.fallback.is_empty() {
        return Err(format!(
            "step '{sid}': fallback: works only with preset tools"
        ));
    }
    if s.continue_from.is_some() && s.cmd.is_some() {
        return Err(format!(
            "step '{sid}': continue_from works only with preset tools, not cmd:"
        ));
    }
    if s.continue_from.is_some() && !s.fallback.is_empty() {
        return Err(format!(
            "step '{sid}': fallback: cannot be combined with continue_from (a session belongs to one tool)"
        ));
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
                "step '{sid}': continue_from cannot be combined with foreach"
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

    /// Returns Err(message) so tests can assert on validation text.
    fn parse(y: &str) -> Result<(), String> {
        let f: Flow = yaml::from_str(y).map_err(|e| e.to_string())?;
        validate(&f)
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
        let e = parse("name: t\nsteps:\n  - id: a\n    foreach: {from: x}\n    cmd: echo hi\n  - id: b\n    tool: claude\n    continue_from: a\n    prompt: x\n").unwrap_err();
        assert!(e.contains("foreach step"), "{e}");
        let e = parse(
            "name: t\nsteps:\n  - id: b\n    tool: claude\n    continue_from: b\n    prompt: x\n",
        )
        .unwrap_err();
        assert!(e.contains("itself"), "{e}");
    }

    #[test]
    fn rejects_sibling_and_duplicate_resume_targets() {
        let y = "name: t\nsteps:\n  - id: seed\n    tool: claude\n    prompt: x\n  - id: g\n    parallel:\n      - id: c1\n        tool: claude\n        continue_from: seed\n        prompt: x\n      - id: c2\n        tool: claude\n        continue_from: seed\n        prompt: x\n";
        let e = parse(y).unwrap_err();
        assert!(e.contains("two concurrent resumes"), "{e}");
    }

    #[test]
    fn accepts_on_max_visits_end() {
        let y = "name: t\nsteps:\n  - id: a\n    cmd: echo hi\n    max_visits: 2\n    on_max_visits: goto:end\n";
        assert!(parse(y).is_ok());
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
}
