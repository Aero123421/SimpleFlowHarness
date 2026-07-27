use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    pub name: Option<String>,
    #[serde(default)]
    pub vars: BTreeMap<String, serde_yaml::Value>,
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
    /// Fail before spawning if a rendered prompt exceeds this many chars.
    pub max_prompt_chars: Option<u64>,
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    /// Profile name from `profiles:` supplying default tool settings.
    #[serde(rename = "use")]
    pub use_: Option<String>,
    /// Preset tool: codex | claude | opencode | grok | agy. Omit to use `cmd`.
    pub tool: Option<String>,
    /// Executable path override for the preset tool (e.g. a specific codex.exe).
    pub bin: Option<String>,
    pub model: Option<String>,
    /// Reasoning effort. codex: model_reasoning_effort, claude: --effort,
    /// opencode: --variant, grok: --reasoning-effort.
    pub effort: Option<String>,
    /// read | write | full (default: write)
    pub access: Option<String>,
    /// Agent name (opencode/claude/grok --agent).
    pub agent: Option<String>,
    /// Extra raw args appended to the preset command line.
    #[serde(default)]
    pub args: Vec<String>,
    /// Custom command instead of a preset. Array = spawned directly (no shell),
    /// String = run through cmd /C (Windows) or sh -c (Unix).
    pub cmd: Option<Cmd>,
    pub prompt: Option<String>,
    /// For custom `cmd` only: "prompt" pipes the rendered prompt to stdin. Default: none.
    pub stdin: Option<String>,
    pub cwd: Option<String>,
    pub timeout_sec: Option<u64>,
    pub max_visits: Option<u32>,
    /// fail (default) | continue | goto:<id> - what to do when the command exits non-zero
    /// or times out. Inside parallel children only fail/continue are allowed.
    pub on_error: Option<String>,
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
    /// Step id, or "end" (finish flow, success) or "fail" (finish flow, failure).
    pub goto: String,
}

pub const TOOLS: [&str; 5] = ["codex", "claude", "opencode", "grok", "agy"];

pub fn load(path: &Path) -> Result<Flow, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut flow: Flow =
        serde_yaml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    merge_global_profiles(&mut flow);
    validate(&flow)?;
    Ok(flow)
}

/// Merge machine-level profiles from ~/.sfh/profiles.yaml (a bare name->profile map).
/// Flow-level profiles win on name conflicts. This keeps flow files portable while
/// machine-specific things (bin: paths, provider/model choices) live outside the repo.
fn merge_global_profiles(flow: &mut Flow) {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    let Some(h) = home else { return };
    let p = std::path::PathBuf::from(h).join(".sfh").join("profiles.yaml");
    let Ok(text) = std::fs::read_to_string(&p) else { return };
    match serde_yaml::from_str::<BTreeMap<String, Profile>>(&text) {
        Ok(globals) => {
            for (k, v) in globals {
                flow.profiles.entry(k).or_insert(v);
            }
        }
        Err(e) => eprintln!("sfh: warning: ignoring {}: {e}", p.display()),
    }
}

impl Flow {
    pub fn vars_string_map(&self) -> Result<BTreeMap<String, String>, String> {
        let mut out = BTreeMap::new();
        for (k, v) in &self.vars {
            let s = match v {
                serde_yaml::Value::String(s) => s.clone(),
                serde_yaml::Value::Number(n) => n.to_string(),
                serde_yaml::Value::Bool(b) => b.to_string(),
                serde_yaml::Value::Null => String::new(),
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
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
}

fn validate(flow: &Flow) -> Result<(), String> {
    if flow.steps.is_empty() {
        return Err("flow has no steps".into());
    }
    let mut seen = HashSet::new();
    for s in &flow.steps {
        if !valid_id(&s.id) {
            return Err(format!("step id '{}' must be non-empty and use only [A-Za-z0-9_-]", s.id));
        }
        if !seen.insert(s.id.clone()) {
            return Err(format!("duplicate step id '{}'", s.id));
        }
        if let Some(children) = &s.parallel {
            for c in children {
                if !valid_id(&c.id) {
                    return Err(format!("step id '{}' must be non-empty and use only [A-Za-z0-9_-]", c.id));
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
    let check_on_error = |s: &Step, is_child: bool| -> Result<(), String> {
        if let Some(oe) = &s.on_error {
            if let Some(g) = oe.strip_prefix("goto:") {
                if g == "end" || g == "fail" {
                    // handled by the engine's on_error arm
                } else if !is_child {
                    check_goto(&format!("step '{}' on_error", s.id), g)?;
                }
            } else if oe != "fail" && oe != "continue" {
                return Err(format!("step '{}': on_error must be fail/continue/goto:<id>", s.id));
            }
        }
        Ok(())
    };
    let check_continue_from = |s: &Step| -> Result<(), String> {
        let Some(cf) = &s.continue_from else { return Ok(()) };
        if cf == &s.id {
            return Err(format!("step '{}': continue_from cannot reference itself", s.id));
        }
        let target = flow.find_step(cf).ok_or_else(|| {
            format!("step '{}': continue_from target '{cf}' is not a step id", s.id)
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
        check_on_error(s, false)?;
        check_continue_from(s)?;
        if let Some(children) = &s.parallel {
            if children.is_empty() {
                return Err(format!("step '{}': parallel: needs at least one child", s.id));
            }
            let child_ids: HashSet<&String> = children.iter().map(|c| &c.id).collect();
            let mut targets_seen: std::collections::HashMap<&String, &String> =
                std::collections::HashMap::new();
            for c in children {
                validate_step(flow, c, true)?;
                check_on_error(c, true)?;
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
            if let Some(rx) = &r.when_matches {
                if !rx.contains("{{") {
                    regex::Regex::new(rx)
                        .map_err(|e| format!("step '{}' route[{i}]: bad regex: {e}", s.id))?;
                }
            }
        }
    }
    Ok(())
}

fn validate_step(flow: &Flow, s: &Step, is_child: bool) -> Result<(), String> {
    let sid = &s.id;
    if is_child {
        if !s.route.is_empty() {
            return Err(format!("step '{sid}': route: is not allowed inside parallel: (put it on the group)"));
        }
        if s.parallel.is_some() || s.foreach.is_some() {
            return Err(format!("step '{sid}': parallel/foreach cannot be nested"));
        }
        if let Some(oe) = &s.on_error {
            if oe.starts_with("goto:") {
                return Err(format!("step '{sid}': on_error goto is not allowed inside parallel: (use fail or continue)"));
            }
        }
        if s.notes.is_some() || s.compact.is_some() || s.max_visits.is_some() {
            return Err(format!(
                "step '{sid}': notes/compact/max_visits are not supported inside parallel: (notes/max_visits go on the group; compact a downstream step)"
            ));
        }
    }
    if s.is_group() {
        if s.tool.is_some() || s.cmd.is_some() || s.prompt.is_some() || s.foreach.is_some()
            || s.continue_from.is_some() || s.use_.is_some() || s.model.is_some()
            || s.stdin.is_some() || s.agent.is_some() || s.compact.is_some()
            || s.bin.is_some() || s.effort.is_some() || s.access.is_some()
            || !s.args.is_empty() || s.cwd.is_some() || s.timeout_sec.is_some()
            || s.max_prompt_chars.is_some()
        {
            return Err(format!(
                "step '{sid}': a parallel: group carries only id/max_parallel/route/on_error/max_visits/notes (tool settings go on the children)"
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
    if s.tool.is_none() && s.cmd.is_none() && profile_tool.is_none() && flow.defaults.tool.is_none() {
        return Err(format!("step '{sid}': needs 'tool' or 'cmd' (or a profile/defaults tool)"));
    }
    if let Some(a) = &s.access {
        if !["read", "write", "full"].contains(&a.as_str()) {
            return Err(format!("step '{sid}': access must be read/write/full, got '{a}'"));
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
    if s.continue_from.is_some() && s.cmd.is_some() {
        return Err(format!("step '{sid}': continue_from works only with preset tools, not cmd:"));
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
            return Err(format!("step '{sid}': continue_from cannot be combined with foreach"));
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
            return Err(format!("step '{sid}': compact needs use: <profile> or tool:"));
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
