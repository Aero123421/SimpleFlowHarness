use crate::{execute, flow, preset, template};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Session recorded for an executed step (continue_from source).
#[derive(Clone)]
pub struct SessionInfo {
    pub tool: String,
    pub id: String,
    /// cwd the step ran in - several tools scope session lookup by directory.
    pub cwd: Option<String>,
    /// Extra identity the tool reports (pi: the session's creation timestamp).
    /// pi accepts any --session-id and silently CREATES a session when the id
    /// is not found in this cwd, so the id alone cannot prove a real resume.
    pub marker: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum RetryMode {
    Transient,
    Any,
    Never,
}

#[derive(Clone, Copy)]
pub struct RetryCfg {
    pub max: u32,
    pub backoff_sec: u64,
    pub mode: RetryMode,
}

impl Default for RetryCfg {
    fn default() -> Self {
        RetryCfg {
            max: 0,
            backoff_sec: 5,
            mode: RetryMode::Transient,
        }
    }
}

/// Everything the engine resolves on the main thread before a leaf runs.
/// Workers only execute; they never touch shared flow state.
#[derive(Clone)]
pub struct Prepared {
    pub tag: String,
    pub inv: execute::Invocation,
    pub parse: preset::OutputParse,
    pub stdin_payload: Option<Vec<u8>>,
    pub cwd: Option<PathBuf>,
    pub timeout: Option<Duration>,
    pub preassigned_session: Option<String>,
    pub expect_session: Option<String>,
    /// On resume: the session marker the tool must report back (see SessionInfo).
    pub expect_marker: Option<String>,
    /// On fork: the parent's session id, which the child must NOT report as its
    /// own - that would mean the fork flag was ignored and this run appended to
    /// the shared parent instead of branching.
    pub forbid_session: Option<String>,
    /// On fork, where the tool reports its parent (pi): positive proof of a branch.
    pub expect_parent: Option<String>,
    /// Steps sharing this key are staggered: the first runs alone so the
    /// provider's prompt cache is warm before the rest start.
    pub warmup_key: Option<String>,
    pub env_remove: Vec<String>,
    pub env_set: Vec<(String, String)>,
    pub out_file: PathBuf,
    pub err_file: PathBuf,
    pub chain_file: PathBuf,
    pub tool: Option<String>,
    pub allow_empty: bool,
    pub retry: RetryCfg,
    pub quiet: bool,
    pub verbose: bool,
}

pub struct LeafDone {
    pub tag: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub interrupted: bool,
    pub dur_ms: u128,
    pub attempts: u32,
    pub chain_output: String,
    pub stderr_clean: String,
    pub out_file: PathBuf,
    pub session_id: Option<String>,
    pub session_marker: Option<String>,
    pub tool: Option<String>,
    pub cwd: Option<String>,
    pub usage: preset::Usage,
    pub cmd: String,
}

impl LeafDone {
    pub fn ok(&self) -> bool {
        self.exit_code == 0 && !self.timed_out && !self.interrupted
    }
}

/// Tool settings after merging step > profile (use:) > defaults. Unrendered.
pub struct Effective {
    pub tool: Option<String>,
    pub bin: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub access: preset::Access,
    pub agent: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub timeout_sec: Option<u64>,
    pub env: BTreeMap<String, String>,
}

/// `profile_override` replaces the step's `use:` (used by fallback:).
pub fn effective_with(
    flow: &flow::Flow,
    step: &flow::Step,
    profile_override: Option<&str>,
) -> Result<Effective, String> {
    let empty = flow::Profile::default();
    let pname = profile_override
        .map(String::from)
        .or_else(|| step.use_.clone());
    let prof = match &pname {
        Some(u) => flow
            .profiles
            .get(u)
            .ok_or_else(|| format!("step '{}': unknown profile '{u}'", step.id))?,
        None => &empty,
    };
    let d = &flow.defaults;
    let access_str = step
        .access
        .as_deref()
        .or(prof.access.as_deref())
        .or(d.access.as_deref());
    let mut args = prof.args.clone();
    args.extend(step.args.iter().cloned());
    let mut env = d.env.clone();
    env.extend(prof.env.clone());
    env.extend(step.env.clone());
    // A fallback profile must be able to replace the tool wholesale.
    let tool = if profile_override.is_some() {
        prof.tool
            .clone()
            .or_else(|| step.tool.clone())
            .or_else(|| d.tool.clone())
    } else {
        step.tool
            .clone()
            .or_else(|| prof.tool.clone())
            .or_else(|| d.tool.clone())
    };
    let model = if profile_override.is_some() {
        prof.model
            .clone()
            .or_else(|| step.model.clone())
            .or_else(|| d.model.clone())
    } else {
        step.model
            .clone()
            .or_else(|| prof.model.clone())
            .or_else(|| d.model.clone())
    };
    Ok(Effective {
        tool,
        bin: if profile_override.is_some() {
            prof.bin.clone().or_else(|| step.bin.clone())
        } else {
            step.bin.clone().or_else(|| prof.bin.clone())
        },
        model,
        effort: step
            .effort
            .clone()
            .or_else(|| prof.effort.clone())
            .or_else(|| d.effort.clone()),
        access: preset::Access::parse(access_str)
            .map_err(|e| format!("step '{}': {e}", step.id))?,
        agent: step.agent.clone().or_else(|| prof.agent.clone()),
        args,
        cwd: step
            .cwd
            .clone()
            .or_else(|| prof.cwd.clone())
            .or_else(|| d.cwd.clone()),
        timeout_sec: step.timeout_sec.or(prof.timeout_sec).or(d.timeout_sec),
        env,
    })
}

pub fn effective(flow: &flow::Flow, step: &flow::Step) -> Result<Effective, String> {
    effective_with(flow, step, None)
}

/// Should a batch of forks off one parent be staggered? Forking only saves money
/// when the provider's prompt cache is already warm, and concurrent children all
/// race the first cache write and miss it.
fn fork_warmup_enabled(flow: &flow::Flow, tool: &str) -> bool {
    match flow.defaults.fork_warmup.as_deref().unwrap_or("auto") {
        "always" => true,
        "never" => false,
        _ => preset::fork_warmup_pays(tool),
    }
}

pub fn retry_cfg(flow: &flow::Flow, step: &flow::Step) -> RetryCfg {
    let r = step.retry.or(flow.defaults.retry);
    let mode = match step
        .retry_on
        .as_deref()
        .or(flow.defaults.retry_on.as_deref())
        .unwrap_or("transient")
    {
        "any" => RetryMode::Any,
        "never" => RetryMode::Never,
        _ => RetryMode::Transient,
    };
    match r {
        Some(r) => RetryCfg {
            max: r.max,
            backoff_sec: r.backoff_sec.unwrap_or(5),
            mode,
        },
        None => RetryCfg {
            max: 0,
            backoff_sec: 5,
            mode,
        },
    }
}

pub struct PrepCtx<'a> {
    pub flow: &'a flow::Flow,
    pub vars: &'a BTreeMap<String, String>,
    pub outputs: &'a BTreeMap<String, template::StepOutput>,
    pub step_ids: &'a HashSet<String>,
    pub run_dir: &'a Path,
    pub flow_dir: &'a Path,
    pub notes_file: &'a Path,
    /// step_id -> session info for executed steps.
    pub sessions: &'a HashMap<String, SessionInfo>,
    /// Steps whose sessions later steps want to resume (continue_from targets).
    pub needed_sessions: &'a HashSet<String>,
    pub quiet: bool,
    pub verbose: bool,
}

pub fn make_builtins(
    cx: &PrepCtx,
    step_id: &str,
    visit: u32,
    prompt_file: &Path,
    extras: &[(&str, String)],
) -> BTreeMap<String, String> {
    let mut b = BTreeMap::new();
    b.insert("run_dir".into(), cx.run_dir.display().to_string());
    b.insert("flow_dir".into(), cx.flow_dir.display().to_string());
    b.insert("step_id".into(), step_id.to_string());
    b.insert("visit".into(), visit.to_string());
    b.insert("os".into(), std::env::consts::OS.to_string());
    b.insert("prompt_file".into(), prompt_file.display().to_string());
    b.insert(
        "notes".into(),
        std::fs::read_to_string(cx.notes_file).unwrap_or_default(),
    );
    for (k, v) in extras {
        b.insert((*k).to_string(), v.clone());
    }
    b
}

const ARG_PROMPT_MAX: usize = 25_000;

/// Render templates, apply guards, and build the concrete command for one leaf run.
pub fn prepare_leaf(
    cx: &PrepCtx,
    step: &flow::Step,
    visit: u32,
    tag: &str,
    extras: &[(&str, String)],
    profile_override: Option<&str>,
) -> Result<Prepared, String> {
    let prompt_file = cx.run_dir.join(format!("{tag}.prompt.txt"));
    let builtins = make_builtins(cx, &step.id, visit, &prompt_file, extras);
    let ctx = template::Ctx {
        vars: cx.vars,
        outputs: cx.outputs,
        step_ids: cx.step_ids,
        builtins,
    };
    let rend = |label: &str, t: &str| -> Result<String, String> {
        template::render(t, &ctx).map_err(|e| format!("step '{}' {label}: {e}", step.id))
    };

    let prompt = match &step.prompt {
        Some(p) => Some(rend("prompt", p)?),
        None => None,
    };
    if let Some(p) = &prompt {
        if p.trim().is_empty() {
            return Err(format!(
                "step '{}': rendered prompt is empty (an upstream step produced no output?)",
                step.id
            ));
        }
        let limit = step
            .max_prompt_chars
            .or(cx.flow.defaults.max_prompt_chars)
            .unwrap_or(u64::MAX);
        let n = p.chars().count() as u64;
        if n > limit {
            return Err(format!(
                "step '{}': rendered prompt is {n} chars, over max_prompt_chars={limit} (add | truncate:N filters or a compact: stage upstream)",
                step.id
            ));
        }
        std::fs::write(&prompt_file, p)
            .map_err(|e| format!("cannot write {}: {e}", prompt_file.display()))?;
    }

    let eff = effective_with(cx.flow, step, profile_override)?;
    let model = opt_rend(&eff.model, &ctx)?;
    let effort = opt_rend(&eff.effort, &ctx)?;
    let agent = opt_rend(&eff.agent, &ctx)?;
    let bin = opt_rend(&eff.bin, &ctx)?;
    let mut args = Vec::new();
    for a in &eff.args {
        args.push(rend("args", a)?);
    }
    let cwd = match &eff.cwd {
        Some(c) => Some(PathBuf::from(rend("cwd", c)?)),
        None => None,
    };
    let timeout_sec = eff.timeout_sec;

    let out_file = cx.run_dir.join(format!("{tag}.out.txt"));
    let err_file = cx.run_dir.join(format!("{tag}.err.txt"));
    let chain_file = cx.run_dir.join(format!("{tag}.chain.txt"));
    let last_file = cx.run_dir.join(format!("{tag}.last.txt"));
    let paths = preset::BuildPaths {
        last_msg: &last_file,
        prompt_file: &prompt_file,
    };

    let mut built: Option<preset::Built> = None;
    let mut forbid_session: Option<String> = None;
    let mut expect_parent: Option<String> = None;
    let mut warmup_key: Option<String> = None;
    let (inv, tool_used) = match &step.cmd {
        Some(flow::Cmd::Shell(s)) => {
            // Substituted values land in a cmd /C | sh -c string; reject anything
            // the shell would re-parse. Escape hatch: argv-form cmd: [...].
            let checked = template::render_checked(s, &ctx, &|key, val| {
                const BAD: &[char] = &[
                    '\n', '\r', '&', '|', '<', '>', '^', '%', '`', '$', ';', '"',
                ];
                if val.contains(BAD) {
                    Err(format!(
                        "substituted value of '{{{{{key}}}}}' contains newlines or shell metacharacters; use an argv-form cmd: [...] or filters (e.g. | head:1)"
                    ))
                } else {
                    Ok(())
                }
            })
            .map_err(|e| format!("step '{}' cmd: {e}", step.id))?;
            (execute::Invocation::Shell(checked), None)
        }
        Some(flow::Cmd::Argv(v)) => {
            let mut nv = Vec::new();
            for x in v {
                nv.push(rend("cmd", x)?);
            }
            (execute::Invocation::Argv(nv), None)
        }
        None => {
            let tool = eff
                .tool
                .clone()
                .ok_or_else(|| format!("step '{}': no tool resolved", step.id))?;
            if tool == "opencode" {
                if let Some(m) = &model {
                    if !m.contains('/') {
                        return Err(format!(
                            "step '{}': opencode model must be provider/model form (got '{m}')",
                            step.id
                        ));
                    }
                }
            }
            for a in &args {
                if preset::ESCALATION_FLAGS.iter().any(|f| a.contains(f)) {
                    eprintln!(
                        "sfh: warning: step '{}': args: contains '{a}', which overrides the declared access level",
                        step.id
                    );
                }
            }
            let inp = preset::PresetInput {
                model,
                effort,
                access: eff.access,
                agent,
                extra: &args,
                bin,
                timeout_sec,
            };
            let session_ref = step.continue_from.as_ref().or(step.fork_from.as_ref());
            let b = if let Some(target) = session_ref {
                let is_fork = step.fork_from.is_some();
                let what = if is_fork {
                    "fork_from"
                } else {
                    "continue_from"
                };
                let info = cx.sessions.get(target).ok_or_else(|| {
                    format!(
                        "step '{}': {what} '{target}' but that step has not produced a session id",
                        step.id
                    )
                })?;
                if info.tool != tool {
                    return Err(format!(
                        "step '{}': {what} '{target}' used tool '{}', this step resolves to '{tool}'",
                        step.id, info.tool
                    ));
                }
                let new_cwd = cwd.as_ref().map(|c| c.display().to_string());
                let cwd_scoped = if is_fork {
                    preset::fork_is_cwd_scoped(&tool)
                } else {
                    preset::session_is_cwd_scoped(&tool)
                };
                if cwd_scoped && info.cwd != new_cwd {
                    eprintln!(
                        "sfh: warning: step '{}': {tool} sessions are cwd-scoped; original ran in {:?}, this step uses {:?} - the session may not be found",
                        step.id, info.cwd, new_cwd
                    );
                }
                if is_fork {
                    let child = gen_uuid();
                    let mut b = preset::build_fork(&tool, &info.id, &child, inp, &paths)?;
                    // Detect a fork flag that was ignored (the run would have
                    // appended to the shared parent) and, on pi, demand the
                    // positive proof it prints.
                    forbid_session = Some(info.id.clone());
                    if tool == "pi" {
                        expect_parent = Some(info.id.clone());
                    }
                    if fork_warmup_enabled(cx.flow, &tool) {
                        warmup_key = Some(format!("{tool}:{}", info.id));
                    }
                    b.expect_marker = None;
                    b
                } else {
                    let mut b = preset::build_resume(&tool, &info.id, inp, &paths)?;
                    b.expect_marker = info.marker.clone();
                    b
                }
            } else {
                let preassign =
                    if cx.needed_sessions.contains(&step.id) && preset::wants_preassign(&tool) {
                        Some(gen_uuid())
                    } else {
                        None
                    };
                preset::build(&tool, inp, &paths, preassign.as_deref())?
            };
            for w in &b.warnings {
                eprintln!("sfh: warning: step '{}': {w}", step.id);
            }
            let inv = execute::Invocation::Argv(b.argv.clone());
            built = Some(b);
            (inv, Some(tool))
        }
    };

    #[allow(clippy::type_complexity)]
    let (parse, delivery, preassigned, expect_session, expect_marker, mut env_remove, mut env_set): (
        preset::OutputParse,
        preset::Delivery,
        Option<String>,
        Option<String>,
        Option<String>,
        Vec<String>,
        Vec<(String, String)>,
    ) = match built {
        Some(b) => (
            b.parse,
            b.delivery,
            b.preassigned_session,
            b.expect_session,
            b.expect_marker,
            b.env_remove,
            b.env_set,
        ),
        None => {
            let d = if step.stdin.as_deref() == Some("prompt") {
                preset::Delivery::Stdin
            } else {
                preset::Delivery::None
            };
            (
                preset::OutputParse::Stdout,
                d,
                None,
                None,
                None,
                Vec::new(),
                Vec::new(),
            )
        }
    };
    env_remove.extend(step.env_remove.iter().cloned());
    for (k, v) in &eff.env {
        env_set.push((k.clone(), rend("env", v)?));
    }

    let (inv, stdin_payload) = match delivery {
        preset::Delivery::Stdin => {
            let p = prompt
                .clone()
                .ok_or_else(|| format!("step '{}': prompt is required", step.id))?;
            (inv, Some(p.into_bytes()))
        }
        preset::Delivery::PromptFile => {
            if prompt.is_none() {
                return Err(format!("step '{}': prompt is required", step.id));
            }
            (inv, None)
        }
        preset::Delivery::Arg => {
            let p = prompt
                .clone()
                .ok_or_else(|| format!("step '{}': prompt is required", step.id))?;
            if p.chars().count() > ARG_PROMPT_MAX {
                return Err(format!(
                    "step '{}': prompt is {} chars but this tool takes it via argv (max {ARG_PROMPT_MAX}); shrink it with filters or compact:, or write it to a file and reference the path",
                    step.id,
                    p.chars().count()
                ));
            }
            match inv {
                execute::Invocation::Argv(mut v) => {
                    v.push(p);
                    (execute::Invocation::Argv(v), None)
                }
                s => (s, None),
            }
        }
        preset::Delivery::None => (inv, None),
    };

    let is_preset = tool_used.is_some();
    Ok(Prepared {
        tag: tag.to_string(),
        inv,
        parse,
        stdin_payload,
        cwd,
        timeout: timeout_sec.map(Duration::from_secs),
        preassigned_session: preassigned,
        expect_session,
        expect_marker,
        forbid_session,
        expect_parent,
        warmup_key,
        env_remove,
        env_set,
        out_file,
        err_file,
        chain_file,
        tool: tool_used,
        // Custom commands may legitimately print nothing; agent steps may not.
        allow_empty: step.allow_empty.unwrap_or(!is_preset),
        retry: retry_cfg(cx.flow, step),
        quiet: cx.quiet,
        verbose: cx.verbose,
    })
}

fn opt_rend(v: &Option<String>, ctx: &template::Ctx) -> Result<Option<String>, String> {
    match v {
        Some(s) => Ok(Some(template::render(s, ctx)?)),
        None => Ok(None),
    }
}

/// Parsed view of one tool run.
#[derive(Default)]
pub struct ParsedOut {
    pub text: String,
    pub session: Option<String>,
    /// Secondary session identity (pi: header timestamp).
    pub session_marker: Option<String>,
    /// Where the tool says this session came from (pi: parent session path).
    pub session_parent: Option<String>,
    pub usage: preset::Usage,
    /// In-band failure the exit code may not reflect.
    pub failed: bool,
}

pub fn parse_output(parse: &preset::OutputParse, stdout: &str, stderr: &str) -> ParsedOut {
    match parse {
        preset::OutputParse::Stdout => ParsedOut {
            text: stdout.trim().to_string(),
            ..Default::default()
        },
        preset::OutputParse::CodexJsonl(f) => {
            let mut o = parse_codex_jsonl(stdout);
            let file_text = std::fs::read_to_string(f).unwrap_or_default();
            if !file_text.trim().is_empty() {
                o.text = file_text.trim().to_string();
            } else if o.text.is_empty() {
                o.text = stdout.trim().to_string();
            }
            if o.session.is_none() {
                o.session = codex_session_from_stderr(stderr);
            }
            o
        }
        preset::OutputParse::ClaudeJson => parse_claude_json(stdout),
        preset::OutputParse::OpencodeNdjson => parse_opencode_ndjson(stdout),
        preset::OutputParse::GrokJson => parse_grok_json(stdout),
        preset::OutputParse::AgyJson => parse_agy_json(stdout),
        preset::OutputParse::PiJsonl => parse_pi_jsonl(stdout),
        preset::OutputParse::CursorJson => parse_cursor_json(stdout),
    }
}

/// cursor-agent --output-format json: one result envelope. A model/API failure
/// emits NO envelope at all and exits non-zero, and `is_error` is always false,
/// so absence of the line is the failure signal - not that field.
fn parse_cursor_json(stdout: &str) -> ParsedOut {
    let mut o = ParsedOut::default();
    let t = stdout.trim();
    let Some(v) = t
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
    else {
        o.text = t.to_string();
        o.failed = !t.is_empty();
        return o;
    };
    o.text = v
        .get("result")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    o.session = v
        .get("session_id")
        .and_then(|x| x.as_str())
        .map(String::from);
    if let Some(u) = v.get("usage") {
        // cumulative, already final - never sum these
        o.usage.input_tokens = num(u.get("inputTokens"));
        o.usage.output_tokens = num(u.get("outputTokens"));
    }
    if v.get("subtype").and_then(|x| x.as_str()) == Some("error") {
        o.failed = true;
    }
    o
}

/// pi --mode json: JSONL. Line 1 is the session header; each turn ends with a
/// message_end. Usage is per message, so it is summed across the run.
fn parse_pi_jsonl(stdout: &str) -> ParsedOut {
    let mut o = ParsedOut::default();
    let (mut inp, mut outp, mut cost) = (0u64, 0u64, 0f64);
    let mut saw_usage = false;
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        match v.get("type").and_then(|x| x.as_str()) {
            Some("session") => {
                o.session = v.get("id").and_then(|x| x.as_str()).map(String::from);
                o.session_marker = v
                    .get("timestamp")
                    .and_then(|x| x.as_str())
                    .map(String::from);
                // Present only on a forked session: the parent's file path.
                o.session_parent = v
                    .get("parentSession")
                    .and_then(|x| x.as_str())
                    .map(String::from);
            }
            Some("message_end") => {
                let Some(m) = v.get("message") else { continue };
                if m.get("role").and_then(|x| x.as_str()) != Some("assistant") {
                    continue;
                }
                // Later assistant messages replace earlier ones (auto-retry).
                o.text = m
                    .get("content")
                    .and_then(|c| c.as_array())
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|b| b.get("type").and_then(|x| x.as_str()) == Some("text"))
                            .filter_map(|b| b.get("text").and_then(|x| x.as_str()))
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                // JSON mode exits 0 even when the model run failed.
                match m.get("stopReason").and_then(|x| x.as_str()) {
                    Some("error") | Some("aborted") => o.failed = true,
                    _ => {}
                }
                if let Some(u) = m.get("usage") {
                    saw_usage = true;
                    inp += u.get("input").and_then(|x| x.as_u64()).unwrap_or(0);
                    outp += u.get("output").and_then(|x| x.as_u64()).unwrap_or(0);
                    cost += u
                        .get("cost")
                        .and_then(|c| c.get("total"))
                        .and_then(|x| x.as_f64())
                        .unwrap_or(0.0);
                }
            }
            _ => {}
        }
    }
    if saw_usage {
        o.usage.input_tokens = Some(inp);
        o.usage.output_tokens = Some(outp);
        o.usage.cost_usd = Some(cost);
    }
    o
}

/// Execute one prepared leaf, honouring its retry policy.
pub fn exec_leaf(prep: Prepared) -> LeafDone {
    let cfg = prep.retry;
    let mut attempt = 0u32;
    loop {
        let mut done = exec_once(prep.clone());
        done.attempts = attempt + 1;
        if done.ok() || done.interrupted || attempt >= cfg.max {
            return done;
        }
        let retryable = match cfg.mode {
            RetryMode::Never => false,
            RetryMode::Any => true,
            RetryMode::Transient => {
                !done.timed_out
                    && execute::is_transient_failure(&done.stderr_clean, &done.chain_output)
            }
        };
        if !retryable {
            return done;
        }
        let wait = cfg.backoff_sec.saturating_mul(1u64 << attempt.min(5));
        if !prep.quiet {
            eprintln!(
                "sfh: [{}] transient failure (exit={}), retrying in {wait}s ({}/{})",
                prep.tag,
                done.exit_code,
                attempt + 1,
                cfg.max
            );
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(wait);
        while std::time::Instant::now() < deadline {
            if execute::interrupted() {
                return done;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        attempt += 1;
    }
}

fn exec_once(p: Prepared) -> LeafDone {
    if !p.quiet {
        eprintln!("sfh: [{}] start", p.tag);
        if p.verbose {
            eprintln!("sfh: [{}] cmd: {}", p.tag, p.inv.describe());
        }
    }
    let cmd_desc = p.inv.describe();
    let cwd_str = p.cwd.as_ref().map(|c| c.display().to_string());
    let outcome = match execute::run_cmd(
        &p.inv,
        p.stdin_payload,
        p.cwd.as_deref(),
        p.timeout,
        &p.env_remove,
        &p.env_set,
    ) {
        Ok(o) => o,
        Err(e) => {
            let _ = std::fs::write(&p.err_file, &e);
            if !p.quiet {
                eprintln!("sfh: [{}] spawn failed: {e}", p.tag);
            }
            return LeafDone {
                tag: p.tag,
                exit_code: -1,
                timed_out: false,
                interrupted: execute::interrupted(),
                dur_ms: 0,
                attempts: 1,
                chain_output: String::new(),
                stderr_clean: e,
                out_file: p.out_file,
                session_id: None,
                session_marker: None,
                tool: p.tool,
                cwd: cwd_str,
                usage: preset::Usage::default(),
                cmd: cmd_desc,
            };
        }
    };
    let stdout_clean = clean_text(&outcome.stdout);
    let mut stderr_clean = clean_text(&outcome.stderr);
    let _ = std::fs::write(&p.out_file, &stdout_clean);
    let _ = std::fs::write(&p.err_file, &stderr_clean);

    let parsed = parse_output(&p.parse, &stdout_clean, &stderr_clean);
    let mut exit_code = outcome.exit_code;
    // Several tools report success/failure in-band and get the exit code wrong.
    if parsed.failed && exit_code == 0 {
        exit_code = 1;
    } else if !parsed.failed
        && exit_code != 0
        && !outcome.timed_out
        && !parsed.text.is_empty()
        && matches!(p.parse, preset::OutputParse::AgyJson)
    {
        exit_code = 0;
    }
    let chain_output = parsed.text;

    let mut session_id = if exit_code == 0 && !outcome.timed_out {
        parsed
            .session
            .clone()
            .or_else(|| p.preassigned_session.clone())
    } else {
        None
    };
    if exit_code == 0 && !outcome.timed_out {
        let mut resume_mismatch = |what: &str, exp: &str, got: &str| {
            exit_code = 1;
            session_id = None;
            stderr_clean.push_str(&format!(
                "\nsfh: resume mismatch: expected {what} '{exp}' but the tool reported '{got}' (it silently started a new session - resuming from a different working directory does this)\n"
            ));
            let _ = std::fs::write(&p.err_file, &stderr_clean);
        };
        if let (Some(exp), Some(got)) = (&p.expect_session, &parsed.session) {
            if got != exp {
                resume_mismatch("session", exp, got);
            }
        }
        // pi accepts any --session-id and creates one when it is not found in
        // this cwd, so the id matching proves nothing; the marker does.
        if let (Some(exp), Some(got)) = (&p.expect_marker, &parsed.session_marker) {
            if got != exp {
                resume_mismatch("session marker", exp, got);
            }
        }
        // A fork that came back as the parent means the fork flag was ignored:
        // this run appended to the session its siblings are also using.
        if let (Some(parent), Some(got)) = (&p.forbid_session, &parsed.session) {
            if got == parent {
                exit_code = 1;
                session_id = None;
                stderr_clean.push_str(&format!(
                    "\nsfh: fork failed: the tool reported the PARENT session '{parent}' instead of a new one, so this run appended to the parent instead of branching\n"
                ));
                let _ = std::fs::write(&p.err_file, &stderr_clean);
            }
        }
        // pi names the parent it branched from - positive proof of a fork.
        if let Some(parent) = &p.expect_parent {
            let ok = parsed
                .session_parent
                .as_deref()
                .map(|sp| sp.contains(parent.as_str()))
                .unwrap_or(false);
            if !ok {
                exit_code = 1;
                session_id = None;
                stderr_clean.push_str(&format!(
                    "\nsfh: fork failed: the new session does not name '{parent}' as its parent (got {:?}), so it did not inherit the parent's context\n",
                    parsed.session_parent
                ));
                let _ = std::fs::write(&p.err_file, &stderr_clean);
            }
        }
        if chain_output.trim().is_empty() && !p.allow_empty {
            exit_code = if exit_code == 0 { 1 } else { exit_code };
            stderr_clean.push_str(
                "\nsfh: the tool exited successfully but produced no final message (set allow_empty: true if that is expected)\n",
            );
            let _ = std::fs::write(&p.err_file, &stderr_clean);
        }
    }
    let _ = std::fs::write(&p.chain_file, &chain_output);

    if !p.quiet {
        let cost = parsed
            .usage
            .cost_usd
            .map(|c| format!(" ${c:.4}"))
            .unwrap_or_default();
        eprintln!(
            "sfh: [{}] exit={}{} {:.1}s output={}ch{cost} -> {}",
            p.tag,
            exit_code,
            if outcome.timed_out { " TIMEOUT" } else { "" },
            outcome.dur_ms as f64 / 1000.0,
            chain_output.chars().count(),
            p.out_file.display(),
        );
    }
    LeafDone {
        tag: p.tag,
        exit_code,
        timed_out: outcome.timed_out,
        interrupted: outcome.interrupted,
        dur_ms: outcome.dur_ms,
        attempts: 1,
        chain_output,
        stderr_clean,
        out_file: p.out_file,
        session_id,
        session_marker: parsed.session_marker,
        tool: p.tool,
        cwd: cwd_str,
        usage: parsed.usage,
        cmd: cmd_desc,
    }
}

fn codex_session_from_stderr(stderr: &str) -> Option<String> {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"(?im)^[ \t]*session id:[ \t]*([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})[ \t]*$",
        )
        .unwrap()
    });
    re.captures_iter(stderr)
        .last()
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

fn num(v: Option<&serde_json::Value>) -> Option<u64> {
    v.and_then(|x| x.as_u64())
}

/// codex --json: JSONL events. thread.started carries the session id,
/// turn.completed the usage; the final text comes from --output-last-message.
fn parse_codex_jsonl(stdout: &str) -> ParsedOut {
    let mut o = ParsedOut::default();
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        match v.get("type").and_then(|x| x.as_str()) {
            Some("thread.started") => {
                if let Some(id) = v.get("thread_id").and_then(|x| x.as_str()) {
                    o.session = Some(id.to_string());
                }
            }
            Some("turn.completed") => {
                if let Some(u) = v.get("usage") {
                    o.usage.input_tokens = num(u.get("input_tokens"));
                    o.usage.output_tokens = num(u.get("output_tokens"));
                }
            }
            Some("turn.failed") => o.failed = true,
            Some("item.completed") => {
                if let Some(item) = v.get("item") {
                    if item.get("type").and_then(|x| x.as_str()) == Some("agent_message") {
                        if let Some(t) = item.get("text").and_then(|x| x.as_str()) {
                            o.text = t.trim().to_string();
                        }
                    }
                }
            }
            _ => {}
        }
    }
    o
}

/// claude --output-format json: one envelope with .result/.session_id/.total_cost_usd.
fn parse_claude_json(stdout: &str) -> ParsedOut {
    let mut o = ParsedOut::default();
    let t = stdout.trim();
    let Some(v) = serde_json::from_str::<serde_json::Value>(t)
        .ok()
        .or_else(|| {
            t.lines()
                .rev()
                .find_map(|l| serde_json::from_str(l.trim()).ok())
        })
    else {
        o.text = t.to_string();
        return o;
    };
    o.text = v
        .get("result")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    o.session = v
        .get("session_id")
        .and_then(|x| x.as_str())
        .map(String::from);
    o.usage.cost_usd = v.get("total_cost_usd").and_then(|x| x.as_f64());
    if let Some(u) = v.get("usage") {
        o.usage.input_tokens = num(u.get("input_tokens"));
        o.usage.output_tokens = num(u.get("output_tokens"));
    }
    o.failed = v.get("is_error").and_then(|x| x.as_bool()).unwrap_or(false);
    o
}

/// opencode --format json: NDJSON events; final answer = concat of `text` events
/// belonging to the last message (dedupe by part id, keep last occurrence).
fn parse_opencode_ndjson(stdout: &str) -> ParsedOut {
    let mut o = ParsedOut::default();
    let mut texts: Vec<(String, String, String)> = Vec::new(); // (part_id, message_id, text)
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if o.session.is_none() {
            if let Some(s) = v.get("sessionID").and_then(|x| x.as_str()) {
                o.session = Some(s.to_string());
            }
        }
        match v.get("type").and_then(|x| x.as_str()) {
            Some("text") => {
                let part = v.get("part");
                let get = |k: &str| {
                    part.and_then(|p| p.get(k))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string()
                };
                let (pid, mid, txt) = (get("id"), get("messageID"), get("text"));
                if let Some(e) = texts
                    .iter_mut()
                    .find(|(id, _, _)| !pid.is_empty() && *id == pid)
                {
                    *e = (pid, mid, txt);
                } else {
                    texts.push((pid, mid, txt));
                }
            }
            Some("step_finish") => {
                if let Some(part) = v.get("part") {
                    if let Some(tk) = part.get("tokens") {
                        o.usage.input_tokens = num(tk.get("input"));
                        o.usage.output_tokens = num(tk.get("output"));
                    }
                    if let Some(c) = part.get("cost").and_then(|x| x.as_f64()) {
                        o.usage.cost_usd = Some(o.usage.cost_usd.unwrap_or(0.0) + c);
                    }
                }
            }
            Some("error") => o.failed = true,
            _ => {}
        }
    }
    let last_mid = texts.last().map(|(_, m, _)| m.clone()).unwrap_or_default();
    o.text = texts
        .iter()
        .filter(|(_, m, _)| *m == last_mid)
        .map(|(_, _, t)| t.as_str())
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string();
    o
}

/// grok --output-format json: one pretty-printed object with .text/.sessionId.
fn parse_grok_json(stdout: &str) -> ParsedOut {
    let mut o = ParsedOut::default();
    let t = stdout.trim();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(t) else {
        o.text = t.to_string();
        return o;
    };
    if v.get("type").and_then(|x| x.as_str()) == Some("error") {
        o.failed = true;
        o.text = v
            .get("message")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        return o;
    }
    o.text = v
        .get("text")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    o.session = v
        .get("sessionId")
        .and_then(|x| x.as_str())
        .map(String::from);
    o.usage.cost_usd = v.get("total_cost_usd").and_then(|x| x.as_f64());
    if let Some(u) = v.get("usage") {
        o.usage.input_tokens = num(u.get("input_tokens"));
        o.usage.output_tokens = num(u.get("output_tokens"));
    }
    o
}

/// agy --output-format json: {response, status, conversation_id, usage};
/// stream-json wraps it as {"event":"result","result":{...}}.
fn parse_agy_json(stdout: &str) -> ParsedOut {
    let mut o = ParsedOut::default();
    let t = stdout.trim();
    let Some(v) = serde_json::from_str::<serde_json::Value>(t)
        .ok()
        .or_else(|| {
            t.lines()
                .rev()
                .find_map(|l| serde_json::from_str(l.trim()).ok())
        })
    else {
        o.text = t.to_string();
        return o;
    };
    let obj = if v.get("event").is_some() {
        v.get("result").cloned().unwrap_or(v)
    } else {
        v
    };
    o.text = obj
        .get("response")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    o.session = obj
        .get("conversation_id")
        .and_then(|x| x.as_str())
        .map(String::from);
    o.failed = obj.get("status").and_then(|x| x.as_str()) == Some("ERROR");
    if let Some(u) = obj.get("usage") {
        o.usage.input_tokens = num(u.get("input_tokens"));
        o.usage.output_tokens = num(u.get("output_tokens"));
    }
    o
}

/// Bounds how many leaves of one tool may run at once across a fan-out.
pub struct ToolGate {
    limits: HashMap<String, u32>,
    state: Mutex<HashMap<String, u32>>,
    cv: Condvar,
}

impl ToolGate {
    pub fn new(limits: HashMap<String, u32>) -> Arc<ToolGate> {
        Arc::new(ToolGate {
            limits,
            state: Mutex::new(HashMap::new()),
            cv: Condvar::new(),
        })
    }
    fn acquire(&self, tool: &Option<String>) -> Option<String> {
        let t = tool.as_ref()?;
        let limit = *self.limits.get(t)?;
        let mut g = self.state.lock().ok()?;
        loop {
            let cur = g.entry(t.clone()).or_insert(0);
            if *cur < limit {
                *cur += 1;
                return Some(t.clone());
            }
            g = self.cv.wait(g).ok()?;
        }
    }
    fn release(&self, held: Option<String>) {
        let Some(t) = held else { return };
        if let Ok(mut g) = self.state.lock() {
            if let Some(c) = g.get_mut(&t) {
                *c = c.saturating_sub(1);
            }
        }
        self.cv.notify_all();
    }
}

/// Staggers leaves that fork the same parent: the first runs alone so the
/// provider's prompt cache is written, then the rest are released together.
/// Without this, N concurrent forks all race the cache write and all miss it
/// (measured on claude: $0.0337 each concurrently vs $0.0026 once warm).
struct Warmup {
    keys: HashSet<String>,
    state: Mutex<HashMap<String, (bool, bool)>>, // key -> (leader_taken, leader_done)
    cv: Condvar,
    quiet: bool,
}

enum WarmRole {
    None,
    Leader(String),
    Follower,
}

impl Warmup {
    fn from(preps: &[Prepared], quiet: bool) -> Warmup {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for p in preps {
            if let Some(k) = &p.warmup_key {
                *counts.entry(k.as_str()).or_insert(0) += 1;
            }
        }
        Warmup {
            keys: counts
                .into_iter()
                .filter(|(_, n)| *n > 1)
                .map(|(k, _)| k.to_string())
                .collect(),
            state: Mutex::new(HashMap::new()),
            cv: Condvar::new(),
            quiet,
        }
    }

    fn enter(&self, p: &Prepared) -> WarmRole {
        let Some(key) = p.warmup_key.as_ref().filter(|k| self.keys.contains(*k)) else {
            return WarmRole::None;
        };
        let Ok(mut g) = self.state.lock() else {
            return WarmRole::None;
        };
        let e = g.entry(key.clone()).or_insert((false, false));
        if !e.0 {
            e.0 = true;
            if !self.quiet {
                eprintln!(
                    "sfh: [{}] warming the fork cache for '{key}' - siblings start when it finishes",
                    p.tag
                );
            }
            return WarmRole::Leader(key.clone());
        }
        while !g.get(key).map(|s| s.1).unwrap_or(true) {
            match self.cv.wait(g) {
                Ok(next) => g = next,
                Err(_) => return WarmRole::None,
            }
        }
        WarmRole::Follower
    }

    fn leave(&self, role: WarmRole) {
        if let WarmRole::Leader(key) = role {
            if let Ok(mut g) = self.state.lock() {
                g.entry(key).or_insert((true, false)).1 = true;
            }
            self.cv.notify_all();
        }
    }
}

/// Run prepared leaves on a bounded worker pool. The result Vec ALWAYS has the
/// same length and order as the input: a slot whose worker died is filled with
/// a synthetic failure instead of being dropped (positional consumers zip these
/// against child/item lists).
pub fn run_pool(preps: Vec<Prepared>, max_parallel: usize, gate: Arc<ToolGate>) -> Vec<LeafDone> {
    let n = preps.len();
    if n == 0 {
        return Vec::new();
    }
    let warmup = Arc::new(Warmup::from(
        &preps,
        preps.first().map(|p| p.quiet).unwrap_or(true),
    ));
    if n == 1 || max_parallel <= 1 {
        return preps
            .into_iter()
            .map(|p| {
                let held = gate.acquire(&p.tool);
                let d = exec_leaf(p);
                gate.release(held);
                d
            })
            .collect();
    }
    let queue: Arc<Mutex<VecDeque<(usize, Prepared)>>> =
        Arc::new(Mutex::new(preps.into_iter().enumerate().collect()));
    let results: Arc<Mutex<Vec<Option<LeafDone>>>> =
        Arc::new(Mutex::new((0..n).map(|_| None).collect()));
    let workers = max_parallel.min(n);
    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let q = Arc::clone(&queue);
        let r = Arc::clone(&results);
        let g = Arc::clone(&gate);
        let w = Arc::clone(&warmup);
        handles.push(std::thread::spawn(move || loop {
            let job = q.lock().unwrap().pop_front();
            let Some((idx, p)) = job else { break };
            let role = w.enter(&p);
            let held = g.acquire(&p.tool);
            let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| exec_leaf(p)))
                .unwrap_or_else(|_| synthetic_failure(idx));
            g.release(held);
            w.leave(role);
            match r.lock() {
                Ok(mut guard) => guard[idx] = Some(done),
                Err(mut poisoned) => poisoned.get_mut()[idx] = Some(done),
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    let slots = Arc::try_unwrap(results)
        .map(|m| m.into_inner().unwrap_or_else(|p| p.into_inner()))
        .unwrap_or_else(|_| (0..n).map(|_| None).collect());
    slots
        .into_iter()
        .enumerate()
        .map(|(i, o)| o.unwrap_or_else(|| synthetic_failure(i)))
        .collect()
}

fn synthetic_failure(idx: usize) -> LeafDone {
    LeafDone {
        tag: format!("slot-{idx}"),
        exit_code: -1,
        timed_out: false,
        interrupted: false,
        dur_ms: 0,
        attempts: 1,
        chain_output: String::new(),
        stderr_clean: "sfh: internal error: worker thread died before producing a result".into(),
        out_file: PathBuf::new(),
        session_id: None,
        session_marker: None,
        tool: None,
        cwd: None,
        usage: preset::Usage::default(),
        cmd: String::new(),
    }
}

/// Format-valid UUIDv4 from OS-seeded hasher entropy (no external crates).
pub fn gen_uuid() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_mut(8) {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        let v = h.finish().to_le_bytes();
        chunk.copy_from_slice(&v[..chunk.len()]);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let h = |r: std::ops::Range<usize>| -> String {
        bytes[r].iter().map(|b| format!("{b:02x}")).collect()
    };
    format!(
        "{}-{}-{}-{}-{}",
        h(0..4),
        h(4..6),
        h(6..8),
        h(8..10),
        h(10..16)
    )
}

pub fn clean_text(b: &[u8]) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"\x1b\[[0-9;:?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][A-Za-z0-9]|\x1b[=>MDEHc78]").unwrap()
    });
    let s = String::from_utf8_lossy(b);
    let s = re.replace_all(&s, "");
    let s = s.replace("\r\n", "\n");
    // Bare CR = progress-bar overwrite: keep only the final frame of each line
    // instead of exploding every frame into its own line.
    let s = s
        .split('\n')
        .map(|line| line.rsplit('\r').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = s.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

pub fn tail_lines(s: &str, n: usize) -> Vec<&str> {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].to_vec()
}

/// Last non-empty line, for deterministic verdict trailers.
pub fn last_line(s: &str) -> &str {
    s.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_strips_ansi_and_collapses_progress_frames() {
        let raw = b"\x1b[32mgreen\x1b[0m\nload 10%\rload 50%\rload 100%\ndone\n";
        assert_eq!(clean_text(raw), "green\nload 100%\ndone\n");
        assert_eq!(
            clean_text(b"\x1b[38:2:255:0:0mtruecolor\x1b[0m\n"),
            "truecolor\n"
        );
        assert_eq!(clean_text(b"a\r\nb\r\n"), "a\nb\n");
        assert_eq!(clean_text(b"   \n\n"), "");
    }

    #[test]
    fn clean_text_survives_invalid_utf8() {
        assert!(clean_text(&[0xff, 0xfe, b'h', b'i']).contains("hi"));
    }

    #[test]
    fn last_line_ignores_trailing_blanks() {
        assert_eq!(last_line("a\nVERDICT: OK\n\n  \n"), "VERDICT: OK");
        assert_eq!(last_line(""), "");
    }

    #[test]
    fn parses_claude_envelope() {
        let s = r#"{"type":"result","result":"hello","session_id":"abc","is_error":false,"total_cost_usd":0.25,"usage":{"input_tokens":10,"output_tokens":3}}"#;
        let o = parse_claude_json(s);
        assert_eq!(o.text, "hello");
        assert_eq!(o.session.as_deref(), Some("abc"));
        assert_eq!(o.usage.cost_usd, Some(0.25));
        assert_eq!(o.usage.input_tokens, Some(10));
        assert!(!o.failed);
        assert!(parse_claude_json(r#"{"result":"x","is_error":true}"#).failed);
    }

    #[test]
    fn parses_opencode_ndjson_and_keeps_last_message_only() {
        let s = concat!(
            r#"{"type":"step_start","sessionID":"ses_1","part":{}}"#,
            "\n",
            r#"{"type":"text","sessionID":"ses_1","part":{"id":"p1","messageID":"m1","text":"old"}}"#,
            "\n",
            r#"{"type":"text","sessionID":"ses_1","part":{"id":"p2","messageID":"m2","text":"new "}}"#,
            "\n",
            r#"{"type":"text","sessionID":"ses_1","part":{"id":"p3","messageID":"m2","text":"answer"}}"#,
            "\n",
            r#"{"type":"step_finish","sessionID":"ses_1","part":{"tokens":{"input":171,"output":6},"cost":0.5}}"#,
        );
        let o = parse_opencode_ndjson(s);
        assert_eq!(o.text, "new answer");
        assert_eq!(o.session.as_deref(), Some("ses_1"));
        assert_eq!(o.usage.input_tokens, Some(171));
        assert_eq!(o.usage.cost_usd, Some(0.5));
    }

    #[test]
    fn opencode_dedupes_streamed_part_updates() {
        let s = concat!(
            r#"{"type":"text","sessionID":"s","part":{"id":"p1","messageID":"m","text":"par"}}"#,
            "\n",
            r#"{"type":"text","sessionID":"s","part":{"id":"p1","messageID":"m","text":"partial full"}}"#,
        );
        assert_eq!(parse_opencode_ndjson(s).text, "partial full");
    }

    #[test]
    fn parses_grok_and_agy_envelopes() {
        let g = parse_grok_json(
            r#"{"text":"BANANA","sessionId":"9ac","total_cost_usd":0.012,"usage":{"input_tokens":5,"output_tokens":2}}"#,
        );
        assert_eq!(g.text, "BANANA");
        assert_eq!(g.session.as_deref(), Some("9ac"));
        assert_eq!(g.usage.cost_usd, Some(0.012));
        assert!(parse_grok_json(r#"{"type":"error","message":"boom"}"#).failed);

        let a = parse_agy_json(
            r#"{"conversation_id":"e3c","status":"SUCCESS","response":"OK\n","usage":{"input_tokens":16913,"output_tokens":38}}"#,
        );
        assert_eq!(a.text, "OK");
        assert_eq!(a.session.as_deref(), Some("e3c"));
        assert_eq!(a.usage.input_tokens, Some(16913));
        assert!(!a.failed);
        assert!(
            parse_agy_json(r#"{"status":"ERROR","error":"empty prompt","response":""}"#).failed
        );
        // stream-json wrapper
        let w = parse_agy_json(
            r#"{"event":"result","result":{"response":"hi","status":"SUCCESS","conversation_id":"c1"}}"#,
        );
        assert_eq!(w.text, "hi");
        assert_eq!(w.session.as_deref(), Some("c1"));
    }

    #[test]
    fn parses_pi_jsonl_text_session_marker_and_summed_usage() {
        let s = concat!(
            r#"{"type":"session","id":"sfh-1","timestamp":"2026-07-27T10:00:00.000Z","cwd":"C:\\w"}"#,
            "\n",
            r#"{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#,
            "\n",
            r#"{"type":"message_end","message":{"role":"assistant","stopReason":"toolUse","content":[{"type":"text","text":"thinking out loud"}],"usage":{"input":100,"output":10,"cost":{"total":0.01}}}}"#,
            "\n",
            r#"{"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[{"type":"thinking","text":"hidden"},{"type":"text","text":"final "},{"type":"text","text":"answer"}],"usage":{"input":200,"output":20,"cost":{"total":0.02}}}}"#,
            "\n",
            r#"{"type":"agent_settled"}"#,
        );
        let o = parse_pi_jsonl(s);
        assert_eq!(
            o.text, "final answer",
            "last assistant message, text blocks only"
        );
        assert_eq!(o.session.as_deref(), Some("sfh-1"));
        assert_eq!(
            o.session_marker.as_deref(),
            Some("2026-07-27T10:00:00.000Z")
        );
        // usage is per message, so a tool-using turn must be summed
        assert_eq!(o.usage.input_tokens, Some(300));
        assert_eq!(o.usage.output_tokens, Some(30));
        assert_eq!(o.usage.cost_usd, Some(0.03));
        assert!(!o.failed);
    }

    #[test]
    fn pi_reports_in_band_failures_that_exit_zero() {
        let err = concat!(
            r#"{"type":"session","id":"s","timestamp":"t"}"#,
            "\n",
            r#"{"type":"message_end","message":{"role":"assistant","stopReason":"error","content":[]}}"#,
        );
        assert!(parse_pi_jsonl(err).failed);
        let aborted = r#"{"type":"message_end","message":{"role":"assistant","stopReason":"aborted","content":[]}}"#;
        assert!(parse_pi_jsonl(aborted).failed);
        // An empty run (no prompt reached the model) yields no text.
        let empty = r#"{"type":"session","id":"s","timestamp":"t"}"#;
        let o = parse_pi_jsonl(empty);
        assert!(o.text.is_empty());
        assert_eq!(o.usage.input_tokens, None);
    }

    #[test]
    fn parses_cursor_envelope_and_treats_a_missing_one_as_failure() {
        let s = r#"{"type":"result","subtype":"success","is_error":false,"result":"hi there","session_id":"c-1","usage":{"inputTokens":120,"outputTokens":7,"cacheReadTokens":900}}"#;
        let o = parse_cursor_json(s);
        assert_eq!(o.text, "hi there");
        assert_eq!(o.session.as_deref(), Some("c-1"));
        assert_eq!(o.usage.input_tokens, Some(120));
        assert_eq!(o.usage.output_tokens, Some(7));
        assert_eq!(o.usage.cost_usd, None, "cursor reports no cost");
        assert!(!o.failed);
        // A model failure prints no envelope at all.
        assert!(parse_cursor_json("Error: something went wrong").failed);
        assert!(
            !parse_cursor_json("").failed,
            "empty output is caught by allow_empty"
        );
        // Leading noise (e.g. the worktree banner) must not hide the envelope.
        let noisy = format!("Using worktree: C:\\tmp\n{s}");
        assert_eq!(parse_cursor_json(&noisy).text, "hi there");
    }

    #[test]
    fn parses_codex_jsonl_session_and_usage() {
        let s = concat!(
            r#"{"type":"thread.started","thread_id":"019fa375-ae0f-7962-bcf6-8682ff388db6"}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"final answer"}}"#,
            "\n",
            r#"{"type":"turn.completed","usage":{"input_tokens":20224,"output_tokens":12}}"#,
        );
        let o = parse_codex_jsonl(s);
        assert_eq!(
            o.session.as_deref(),
            Some("019fa375-ae0f-7962-bcf6-8682ff388db6")
        );
        assert_eq!(o.text, "final answer");
        assert_eq!(o.usage.input_tokens, Some(20224));
        assert!(!o.failed);
        assert!(parse_codex_jsonl(r#"{"type":"turn.failed"}"#).failed);
    }

    #[test]
    fn codex_stderr_regex_requires_a_real_uuid_on_its_own_line() {
        let ok = "  session id: 019fa375-ae0f-7962-bcf6-8682ff388db6  \n";
        assert!(codex_session_from_stderr(ok).is_some());
        // A prose mention must not be scraped.
        assert!(codex_session_from_stderr("the session id: not-a-uuid here\n").is_none());
        assert!(
            codex_session_from_stderr("session id:\n019fa375-ae0f-7962-bcf6-8682ff388db6\n")
                .is_none()
        );
    }

    #[test]
    fn gen_uuid_is_v4_shaped_and_unique() {
        let a = gen_uuid();
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        let mut set = std::collections::HashSet::new();
        for _ in 0..200 {
            assert!(set.insert(gen_uuid()), "uuid collision");
        }
    }

    #[test]
    fn tool_gate_bounds_concurrency_per_tool() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let gate = ToolGate::new(HashMap::from([("t".to_string(), 2u32)]));
        let live = Arc::new(AtomicU32::new(0));
        let peak = Arc::new(AtomicU32::new(0));
        let mut hs = Vec::new();
        for _ in 0..8 {
            let (g, l, p) = (Arc::clone(&gate), Arc::clone(&live), Arc::clone(&peak));
            hs.push(std::thread::spawn(move || {
                let held = g.acquire(&Some("t".to_string()));
                let cur = l.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(cur, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
                l.fetch_sub(1, Ordering::SeqCst);
                g.release(held);
            }));
        }
        for h in hs {
            h.join().unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) <= 2, "gate exceeded its limit");
    }
}
