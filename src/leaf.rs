use crate::{execute, flow, preset, template};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Session recorded for an executed step (continue_from source).
#[derive(Clone)]
pub struct SessionInfo {
    pub tool: String,
    pub id: String,
    /// cwd the step ran in - claude/grok/agy scope session lookup by directory.
    pub cwd: Option<String>,
}

/// Everything the engine resolves on the main thread before a leaf runs.
/// Workers only execute; they never touch shared flow state.
pub struct Prepared {
    pub tag: String,
    pub inv: execute::Invocation,
    pub parse: preset::OutputParse,
    pub stdin_payload: Option<Vec<u8>>,
    pub cwd: Option<PathBuf>,
    pub timeout: Option<Duration>,
    pub session: preset::SessionCapture,
    pub expect_session: Option<String>,
    pub env_remove: Vec<String>,
    pub env_set: Vec<(String, String)>,
    pub out_file: PathBuf,
    pub err_file: PathBuf,
    pub tool: Option<String>,
    pub quiet: bool,
    pub verbose: bool,
}

pub struct LeafDone {
    pub tag: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub dur_ms: u128,
    pub chain_output: String,
    pub stderr_clean: String,
    pub out_file: PathBuf,
    pub session_id: Option<String>,
    pub tool: Option<String>,
    pub cwd: Option<String>,
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
}

pub fn effective(flow: &flow::Flow, step: &flow::Step) -> Result<Effective, String> {
    let empty = flow::Profile::default();
    let prof = match &step.use_ {
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
    Ok(Effective {
        tool: step.tool.clone().or_else(|| prof.tool.clone()).or_else(|| d.tool.clone()),
        bin: step.bin.clone().or_else(|| prof.bin.clone()),
        model: step.model.clone().or_else(|| prof.model.clone()).or_else(|| d.model.clone()),
        effort: step.effort.clone().or_else(|| prof.effort.clone()).or_else(|| d.effort.clone()),
        access: preset::Access::parse(access_str).map_err(|e| format!("step '{}': {e}", step.id))?,
        agent: step.agent.clone().or_else(|| prof.agent.clone()),
        args,
        cwd: step.cwd.clone().or_else(|| prof.cwd.clone()).or_else(|| d.cwd.clone()),
        timeout_sec: step.timeout_sec.or(prof.timeout_sec).or(d.timeout_sec),
    })
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

    let eff = effective(cx.flow, step)?;
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
    let last_file = cx.run_dir.join(format!("{tag}.last.txt"));
    let paths = preset::BuildPaths {
        last_msg: &last_file,
        prompt_file: &prompt_file,
    };

    let mut built: Option<preset::Built> = None;
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
            let inp = preset::PresetInput {
                model,
                effort,
                access: eff.access,
                agent,
                extra: &args,
                bin,
                timeout_sec,
            };
            let b = if let Some(target) = &step.continue_from {
                let info = cx.sessions.get(target).ok_or_else(|| {
                    format!(
                        "step '{}': continue_from '{target}' but that step has not produced a session id",
                        step.id
                    )
                })?;
                if info.tool != tool {
                    return Err(format!(
                        "step '{}': continue_from '{target}' used tool '{}', this step resolves to '{tool}'",
                        step.id, info.tool
                    ));
                }
                let new_cwd = cwd.as_ref().map(|c| c.display().to_string());
                if matches!(tool.as_str(), "claude" | "grok" | "agy") && info.cwd != new_cwd {
                    eprintln!(
                        "sfh: warning: step '{}': {tool} sessions are cwd-scoped; original ran in {:?}, this step uses {:?} - the session may not be found",
                        step.id, info.cwd, new_cwd
                    );
                }
                preset::build_resume(&tool, &info.id, inp, &paths)?
            } else {
                let want_capture = cx.needed_sessions.contains(&step.id);
                let preassign = if want_capture && preset::wants_preassign(&tool) {
                    Some(gen_uuid())
                } else {
                    None
                };
                preset::build(&tool, inp, &paths, preassign.as_deref(), want_capture)?
            };
            for w in &b.warnings {
                eprintln!("sfh: warning: step '{}': {w}", step.id);
            }
            let inv = execute::Invocation::Argv(b.argv.clone());
            built = Some(b);
            (inv, Some(tool))
        }
    };

    let (parse, delivery, session, expect_session, env_remove, env_set) = match built {
        Some(b) => (b.parse, b.delivery, b.session, b.expect_session, b.env_remove, b.env_set),
        None => {
            let d = if step.stdin.as_deref() == Some("prompt") {
                preset::Delivery::Stdin
            } else {
                preset::Delivery::None
            };
            (
                preset::OutputParse::Stdout,
                d,
                preset::SessionCapture::Unsupported,
                None,
                Vec::new(),
                Vec::new(),
            )
        }
    };

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

    Ok(Prepared {
        tag: tag.to_string(),
        inv,
        parse,
        stdin_payload,
        cwd,
        timeout: timeout_sec.map(Duration::from_secs),
        session,
        expect_session,
        env_remove,
        env_set,
        out_file,
        err_file,
        tool: tool_used,
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

/// Execute one prepared leaf. Thread-safe: touches only its own files.
pub fn exec_leaf(p: Prepared) -> LeafDone {
    if !p.quiet {
        eprintln!("sfh: [{}] start", p.tag);
        if p.verbose {
            eprintln!("sfh: [{}] cmd: {}", p.tag, p.inv.describe());
        }
    }
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
                dur_ms: 0,
                chain_output: String::new(),
                stderr_clean: e,
                out_file: p.out_file,
                session_id: None,
                tool: p.tool,
                cwd: cwd_str,
            };
        }
    };
    let stdout_clean = clean_text(&outcome.stdout);
    let mut stderr_clean = clean_text(&outcome.stderr);
    let _ = std::fs::write(&p.out_file, &stdout_clean);
    let _ = std::fs::write(&p.err_file, &stderr_clean);

    let mut exit_code = outcome.exit_code;
    let mut session_from_parse: Option<String> = None;
    let chain_output = match &p.parse {
        preset::OutputParse::Stdout => stdout_clean.trim().to_string(),
        preset::OutputParse::LastMsgFile(f) => match std::fs::read_to_string(f) {
            Ok(s) if !s.trim().is_empty() => s.trim().to_string(),
            _ => stdout_clean.trim().to_string(),
        },
        preset::OutputParse::OpencodeNdjson => {
            let (text, sid) = parse_opencode_ndjson(&stdout_clean);
            session_from_parse = sid;
            text
        }
        preset::OutputParse::AgyJson => match parse_agy_json(&stdout_clean) {
            Some((resp, status, cid)) => {
                session_from_parse = cid;
                // agy can exit 1 while still producing a valid completion, and
                // json mode reports errors in-band; trust the envelope status.
                if status == "SUCCESS" && exit_code != 0 && !outcome.timed_out {
                    exit_code = 0;
                }
                if status == "ERROR" && exit_code == 0 {
                    exit_code = 1;
                }
                resp
            }
            None => stdout_clean.trim().to_string(),
        },
    };

    let ok = exit_code == 0 && !outcome.timed_out;
    let mut session_id = if ok {
        match &p.session {
            // For codex resumes prefer a freshly parsed id when the tool printed
            // one (resume keeps its id today, but don't build a chain on that
            // assumption). Other tools' ids are trusted verbatim - the codex
            // scraper must never override them.
            preset::SessionCapture::Preassigned(id) => {
                if p.tool.as_deref() == Some("codex") {
                    Some(codex_session_from_stderr(&stderr_clean).unwrap_or_else(|| id.clone()))
                } else {
                    Some(id.clone())
                }
            }
            preset::SessionCapture::CodexStderr => codex_session_from_stderr(&stderr_clean),
            preset::SessionCapture::OpencodeNdjson | preset::SessionCapture::AgyJson => {
                session_from_parse.clone()
            }
            preset::SessionCapture::Unsupported => None,
        }
    } else {
        None
    };
    if ok {
        if let Some(exp) = &p.expect_session {
            let got = session_from_parse.clone().unwrap_or_default();
            if got != *exp {
                exit_code = 1;
                session_id = None;
                stderr_clean.push_str(&format!(
                    "\nsfh: resume mismatch: expected conversation '{exp}' but the tool reported '{got}' (it silently started a new conversation)\n"
                ));
                let _ = std::fs::write(&p.err_file, &stderr_clean);
            }
        }
    }

    if !p.quiet {
        eprintln!(
            "sfh: [{}] exit={}{} {:.1}s output={}ch -> {}",
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
        dur_ms: outcome.dur_ms,
        chain_output,
        stderr_clean,
        out_file: p.out_file,
        session_id,
        tool: p.tool,
        cwd: cwd_str,
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

/// opencode --format json: NDJSON events; final answer = concat of `text` events
/// belonging to the last message (dedupe by part id, keep last occurrence).
fn parse_opencode_ndjson(stdout: &str) -> (String, Option<String>) {
    let mut session: Option<String> = None;
    let mut texts: Vec<(String, String, String)> = Vec::new(); // (part_id, message_id, text)
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if session.is_none() {
            if let Some(s) = v.get("sessionID").and_then(|x| x.as_str()) {
                session = Some(s.to_string());
            }
        }
        if v.get("type").and_then(|x| x.as_str()) == Some("text") {
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
    }
    let last_mid = texts.last().map(|(_, m, _)| m.clone()).unwrap_or_default();
    let text = texts
        .iter()
        .filter(|(_, m, _)| *m == last_mid)
        .map(|(_, _, t)| t.as_str())
        .collect::<Vec<_>>()
        .join("");
    (text.trim().to_string(), session)
}

/// agy --output-format json: one JSON envelope {response, status, conversation_id, ...};
/// stream-json wraps it as {"event":"result","result":{...}}.
fn parse_agy_json(stdout: &str) -> Option<(String, String, Option<String>)> {
    let t = stdout.trim();
    let v: serde_json::Value = serde_json::from_str(t).ok().or_else(|| {
        t.lines()
            .rev()
            .find_map(|l| serde_json::from_str(l.trim()).ok())
    })?;
    let obj = if v.get("event").is_some() {
        v.get("result")?.clone()
    } else {
        v
    };
    let resp = obj
        .get("response")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim_end()
        .to_string();
    let status = obj
        .get("status")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let cid = obj
        .get("conversation_id")
        .and_then(|x| x.as_str())
        .map(String::from);
    Some((resp, status, cid))
}

/// Run prepared leaves on a bounded worker pool. The result Vec ALWAYS has the
/// same length and order as the input: a slot whose worker died is filled with
/// a synthetic failure instead of being dropped (positional consumers zip these
/// against child/item lists).
pub fn run_pool(preps: Vec<Prepared>, max_parallel: usize) -> Vec<LeafDone> {
    let n = preps.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 || max_parallel <= 1 {
        return preps.into_iter().map(exec_leaf).collect();
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
        handles.push(std::thread::spawn(move || loop {
            let job = q.lock().unwrap().pop_front();
            let Some((idx, p)) = job else { break };
            let done =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| exec_leaf(p)))
                    .unwrap_or_else(|_| synthetic_failure(idx));
            match r.lock() {
                Ok(mut g) => g[idx] = Some(done),
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
        dur_ms: 0,
        chain_output: String::new(),
        stderr_clean: "sfh: internal error: worker thread died before producing a result".into(),
        out_file: std::path::PathBuf::new(),
        session_id: None,
        tool: None,
        cwd: None,
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
    format!("{}-{}-{}-{}-{}", h(0..4), h(4..6), h(6..8), h(8..10), h(10..16))
}

pub fn clean_text(b: &[u8]) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)").unwrap()
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
