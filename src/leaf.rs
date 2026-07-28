use crate::{contain, execute, flow, preset, template};
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
    /// Access level the session was created under. A later step may not resume
    /// or fork it at a HIGHER level (untrusted context ingested at read must
    /// not be promoted to write/full). None = not known.
    pub access: Option<preset::Access>,
    /// Whether the log carried an `access` key at all, which is NOT the same
    /// question as whether it parsed. A pre-1.0 run honestly has no key and its
    /// level can be filled in from the flow; a run that has the key but whose
    /// value is `"bogus"` or `null` has been edited, and no amount of claiming
    /// to be old should get it the same treatment. Collapsing both to None let
    /// a forged `sfh_version: 0.x` launder a tampered level.
    pub access_recorded: bool,
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
    pub run_dir: PathBuf,
    pub out_file: PathBuf,
    pub err_file: PathBuf,
    pub chain_file: PathBuf,
    pub tool: Option<String>,
    /// Declared access of this run (preset steps only); recorded with the
    /// session so later steps cannot resume it at a higher level.
    pub access: Option<preset::Access>,
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
    /// Declared access this run executed under (preset steps only).
    pub access: Option<preset::Access>,
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
    /// Var keys whose values came from a resumed run dir's meta.json and were
    /// not overridden by an explicit --var: run-derived UNTRUSTED input, barred
    /// from executed-privileged template sinks (rev_break #12).
    pub tainted_vars: &'a HashSet<String>,
    pub quiet: bool,
    pub verbose: bool,
}

/// Whether a template key may feed an EXECUTED-privileged sink: `bin` (argv[0]),
/// `cwd` (the workspace base a write/full tool acts in) and argv[0] of a custom
/// cmd. These are executed with sfh's own OS rights regardless of access: read,
/// so only values the USER controls may flow into them (rev_break #12):
/// - `steps.*` is step output - untrusted on a fresh run (an upstream or resumed
///   result could name an arbitrary binary or move the write base),
/// - `notes` / `item` / `item_index` are run-derived the same way (notes.md is
///   appended from step output; foreach items render from it),
/// - a var is barred only when its value came from the resumed run dir's
///   meta.json (tainted) and was not re-supplied with --var; a var the user
///   defined or overrode is their own value and stays allowed.
///
/// A step can opt back in with allow_dynamic_exec_paths: true, in which case
/// the caller does not consult this predicate at all.
pub fn exec_path_key_check(key: &str, tainted_vars: &HashSet<String>) -> Result<(), String> {
    if key.starts_with("steps.") {
        return Err(format!(
            "refusing to expand '{{{{{key}}}}}': this field is executed by sfh and must not depend on step output (an upstream or resumed result could inject an arbitrary path). Set allow_dynamic_exec_paths: true on this step to accept run-derived values here"
        ));
    }
    if matches!(key, "notes" | "item" | "item_index") {
        return Err(format!(
            "refusing to expand '{{{{{key}}}}}': this field is executed by sfh and '{key}' is run-derived data (set allow_dynamic_exec_paths: true on this step to accept it)"
        ));
    }
    if let Some(name) = key.strip_prefix("vars.") {
        if tainted_vars.contains(name) {
            return Err(format!(
                "refusing to expand '{{{{{key}}}}}': this field is executed by sfh and var '{name}' came from the resumed run dir's meta.json (pass --var {name}=... to supply your own value, or set allow_dynamic_exec_paths: true on this step)"
            ));
        }
    }
    Ok(())
}

pub fn exec_template_check<'a>(
    tainted_vars: &'a HashSet<String>,
) -> impl Fn(&str, &str) -> Result<(), String> + 'a {
    move |key: &str, _: &str| -> Result<(), String> { exec_path_key_check(key, tainted_vars) }
}

/// Which arguments of `argv` are shell TEXT, when argv[0] is a shell and one of
/// its "run this string" flags is present. That text is re-parsed by the shell,
/// so it needs the same template treatment as a string-form cmd - the argv
/// branch's ordinary "data is safe" reasoning does not apply to it (rev_break
/// #13: `cmd: ["sh","-c","...{{x}}..."]` bypassed the shell-template defence).
///
/// A RANGE, not a start index, because the shells disagree about what follows
/// the flag and the difference decides whether the safest way to write this is
/// allowed or refused:
///
/// - `sh -c SCRIPT name arg1 arg2` - only SCRIPT is shell text. The rest become
///   `$0 $1 $2` INSIDE the script and are never re-parsed. That is the standard
///   way to hand untrusted data to a shell safely, and treating the whole tail
///   as script text refused exactly the flows that were doing the right thing.
///   The value still arrives as one word whatever it contains; the script has
///   to write `"$1"` rather than `$1` to keep it that way, which is the same
///   trust the documented `cmd: ["program", "--flag", "{{x}}"]` form already
///   places in the program being handed the argument.
/// - `cmd /c ...` and `powershell -Command ...` - the remaining arguments are
///   joined back into one command line, so all of them are shell text.
/// - `powershell -EncodedCommand B64` - one argument, like sh.
pub fn shell_script_span(argv: &[String]) -> Option<std::ops::Range<usize>> {
    let prog = argv.first()?;
    // Split on BOTH separators by hand rather than using Path::file_stem. A
    // security check must give the same answer on every OS, and file_stem does
    // not: on Linux, `C:\Windows\System32\cmd.exe` has no separator at all, so
    // the whole string is the file name and the shell goes unrecognised. The
    // flow file is the same on all three platforms, so the verdict must be too.
    let base = prog
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(prog)
        .to_lowercase();
    // Only the Windows executable suffix comes off. Stripping whatever follows
    // the last dot turned `sh.py` - an ordinary script that happens to be named
    // after a shell - into "sh", and refused templates in its arguments for no
    // reason. `.exe` is the only extension that actually makes cmd.exe the same
    // program as cmd.
    let stem = base.strip_suffix(".exe").unwrap_or(base.as_str());
    let sh_family = matches!(
        stem,
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "ash" | "busybox"
    );
    let cmd_exe = stem == "cmd";
    // PowerShell was the one shell family this check did not know about, so
    // `cmd: ["pwsh","-Command","...{{untrusted}}..."]` walked straight past it.
    // pwsh ships for macOS and Linux too, so this is not a Windows-only gap.
    let powershell = matches!(stem, "powershell" | "pwsh");
    if !sh_family && !cmd_exe && !powershell {
        return None;
    }
    for (i, a) in argv.iter().enumerate().skip(1) {
        let low = a.to_lowercase();
        if sh_family && (low == "-c" || low == "-lc" || low == "-ec") {
            return Some(i + 1..(i + 2).min(argv.len()));
        }
        if cmd_exe && (low == "/c" || low == "/k" || low == "/r") {
            return Some(i + 1..argv.len());
        }
        // PowerShell takes any unambiguous prefix of a switch name and either
        // introducer, so -c, -Com and /Command all mean -Command. Match the
        // same way instead of listing spellings - a list would miss -Comm and
        // let it through. The other switches do not collide: "executionpolicy"
        // and "configurationname" are not prefixes of either name.
        if powershell && (low.starts_with('-') || low.starts_with('/')) {
            let bare = low.trim_start_matches(['-', '/']);
            if bare.is_empty() {
                continue;
            }
            // -EncodedCommand takes exactly one base64 argument; -Command takes
            // everything left. Check the longer name first: "c" is a prefix of
            // "command" only, so the order matters just for the "e..." spellings.
            if "encodedcommand".starts_with(bare) && bare.starts_with('e') {
                return Some(i + 1..(i + 2).min(argv.len()));
            }
            if "command".starts_with(bare) {
                return Some(i + 1..argv.len());
            }
        }
    }
    None
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

/// Metacharacter check applied to substituted values when a step opts in to
/// shell templating (unsafe_shell_template), for both the string-form cmd and
/// an argv form that wraps a shell. A delimiter filter, not a security
/// boundary - see the callers for why expansion is refused without the opt-in.
pub fn shell_metachar_check(key: &str, val: &str) -> Result<(), String> {
    const BAD: &[char] = &['\n', '\r', '&', '|', '<', '>', '^', '%', '`', '$', ';', '"'];
    if val.contains(BAD) {
        Err(format!(
            "substituted value of '{{{{{key}}}}}' contains newlines or shell metacharacters; use an argv-form cmd: [...] or filters (e.g. | head:1)"
        ))
    } else {
        Ok(())
    }
}

/// The refusal text for template expansion that would land in shell-parsed text.
pub fn shell_expansion_refused(what: &str, key: &str) -> String {
    // Lead with the positional-argument form. When a step genuinely needs a
    // shell - a loop, a pipe, a conditional - "avoid the shell" is not an
    // answer and the reader goes straight to unsafe_shell_template, which is
    // the one option here that is actually unsafe. Arguments after `sh -c
    // SCRIPT` arrive as $1, $2 ... inside the script and are never re-parsed,
    // so they carry an untrusted value safely.
    format!(
        "{what} would expand '{{{{{key}}}}}' into a shell string, and template expansion in a shell-parsed cmd is disabled by default (the substituted value would be re-parsed by the shell). Pass it as an argument instead of splicing it into the script:\n  cmd: [\"sh\", \"-c\", \"grep -- \\\"$1\\\" file\", \"step-name\", \"{{{{{key}}}}}\"]\nor, if no shell is needed at all:\n  cmd: [\"program\", \"--flag\", \"{{{{{key}}}}}\"]\nSetting unsafe_shell_template: true accepts shell templating, with only a metacharacter filter that a hostile value can still get past"
    )
}

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
        contain::write_private(&prompt_file, p)
            .map_err(|e| format!("cannot write {}: {e}", prompt_file.display()))?;
    }

    let eff = effective_with(cx.flow, step, profile_override)?;
    let model = opt_rend(&eff.model, &ctx)?;
    let effort = opt_rend(&eff.effort, &ctx)?;
    let agent = opt_rend(&eff.agent, &ctx)?;
    // `bin` becomes argv[0] and `cwd` is the workspace base a write/full tool
    // acts in, so both are EXECUTED-privileged: an upstream or resumed step
    // output flowing into either would let untrusted text point sfh at an
    // arbitrary binary (bin) or move the write base outside the workspace (cwd),
    // in both cases running with sfh's own OS rights regardless of access: read.
    // Refuse run-derived templates in these fields (step output, notes, foreach
    // items, and vars restored from the resumed run dir); user-controlled
    // sources still expand (rev_break #9, rev_break #12). The step can opt back
    // in with allow_dynamic_exec_paths: true.
    let rend_exec = |label: &str, t: &str| -> Result<String, String> {
        if step.allow_dynamic_exec_paths.unwrap_or(false) {
            return template::render(t, &ctx)
                .map_err(|e| format!("step '{}' {label}: {e}", step.id));
        }
        template::render_checked(t, &ctx, &exec_template_check(cx.tainted_vars))
            .map_err(|e| format!("step '{}' {label}: {e}", step.id))
    };
    let bin = match &eff.bin {
        Some(b) => Some(rend_exec("bin", b)?),
        None => None,
    };
    let mut args = Vec::new();
    for a in &eff.args {
        args.push(rend("args", a)?);
    }
    let cwd = match &eff.cwd {
        Some(c) => Some(PathBuf::from(rend_exec("cwd", c)?)),
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
            // Substituted values land in a cmd /C | sh -c string. By default
            // ANY expansion is refused: the metacharacter blacklist below is a
            // delimiter filter, not a security boundary - a hostile value can
            // be a dangerous option to the target program (tar
            // --checkpoint-action=exec=...) without containing a single banned
            // character. The safe path is the argv form, which never touches a
            // shell; unsafe_shell_template: true is the explicit opt-in back
            // into shell templating (with the metacharacter check still on).
            // cx.flow.legacy_resume: the lenient loader accepted this flow for
            // a 0.x resume and already warned about the template. Refusing it
            // again at execution time made the warning meaningless and left the
            // old run unresumable. The metacharacter filter still runs.
            let checked = if step.unsafe_shell_template.unwrap_or(false) || cx.flow.legacy_resume {
                template::render_checked(s, &ctx, &shell_metachar_check)
            } else {
                template::render_checked(s, &ctx, &|key, _| {
                    Err(shell_expansion_refused("string-form cmd", key))
                })
            }
            .map_err(|e| format!("step '{}' cmd: {e}", step.id))?;
            (execute::Invocation::Shell(checked), None)
        }
        Some(flow::Cmd::Argv(v)) => {
            if v.is_empty() {
                return Err(format!("step '{}': cmd array is empty", step.id));
            }
            // argv[0] is the program sfh executes: the same executed-privileged
            // sink as `bin`, so it gets the same run-derived-template refusal
            // (rev_break #12 - the old code rendered argv[0] like data, so a
            // crafted run could select an arbitrary program through a resumed
            // meta.json var or a step output).
            let mut nv = vec![rend_exec("cmd[0]", &v[0])?];
            // An argv form that WRAPS a shell (["sh","-c","..."]) re-parses its
            // script argument in that shell, so the script gets the exact same
            // treatment as a string-form cmd: expansion refused by default,
            // metacharacter-checked under unsafe_shell_template. The old code
            // saw only "the argv branch" here and skipped every shell defence
            // (rev_break #13).
            let mut head = nv.clone();
            head.extend(v.iter().skip(1).cloned());
            let script_span = shell_script_span(&head);
            for (i, x) in v.iter().enumerate().skip(1) {
                let rendered = match &script_span {
                    Some(span) if span.contains(&i) => {
                        if step.unsafe_shell_template.unwrap_or(false) || cx.flow.legacy_resume {
                            template::render_checked(x, &ctx, &shell_metachar_check)
                        } else {
                            template::render_checked(x, &ctx, &|key, _| {
                                Err(shell_expansion_refused(
                                    "cmd: [\"sh\", \"-c\", ...] shell text",
                                    key,
                                ))
                            })
                        }
                    }
                    _ => rend("cmd", x),
                };
                nv.push(rendered.map_err(|e| format!("step '{}' cmd: {e}", step.id))?);
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
            // Same check validation runs on the literal args, repeated here on
            // the RENDERED args: args may contain templates, so an upstream
            // output can inject a permission flag that no load-time check saw.
            // Fail-closed: refuse instead of warn, unless the step opted in.
            if !matches!(eff.access, preset::Access::Full)
                && !step.allow_access_override.unwrap_or(false)
            {
                if let Some(e) = preset::find_escalation(&tool, eff.access, &args) {
                    return Err(preset::escalation_error(&step.id, eff.access, &e));
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
                // A session ingested at low access may hold untrusted content
                // (e.g. a web page read during a read step); resuming it at a
                // higher tier promotes that content into a privileged agent.
                // Refuse the escalation unless the step explicitly opts in.
                match info.access {
                    Some(prev)
                        if eff.access.rank() > prev.rank()
                            && !step.allow_access_override.unwrap_or(false) =>
                    {
                        return Err(format!(
                            "step '{}': {what} '{target}' ran with access {}, but this step declares {} - refusing to resume a session at a higher access level than it was created (set allow_access_override: true on this step to accept)",
                            step.id,
                            prev.as_str(),
                            eff.access.as_str()
                        ));
                    }
                    // Missing OR UNPARSEABLE recorded access (a run dir that
                    // predates the recording, or a log.jsonl an attacker edited
                    // to drop the field). Warn-and-continue would be fail-open:
                    // deleting one field from an attacker-controlled run dir
                    // would be enough to resume a read session at full. So this
                    // is fail-closed at EVERY tier, read included, unless the
                    // step explicitly opted in (rev_complete S2-4). "But read is
                    // the lowest tier, so nothing can escalate into it" is true
                    // for an HONEST missing field, yet indistinguishable from a
                    // field an attacker deleted, and a read resume of a session
                    // whose true level is unknown still re-enters a context sfh
                    // cannot vouch for - so the opt-in is required either way.
                    // Legitimate pre-1.0 runs are not punished by this: the
                    // engine fills their missing levels from the (fingerprint-
                    // verified) flow before this guard runs (reconcile_session_
                    // access), so None here means a recording-era run with a
                    // missing or altered field, not an old run.
                    None => {
                        if step.allow_access_override.unwrap_or(false) {
                            eprintln!(
                                "sfh: warning: step '{}': the session of '{target}' has no recorded access level (the log was altered, or a pre-1.0 run is being resumed without its flow); proceeding because allow_access_override is set",
                                step.id
                            );
                        } else {
                            return Err(format!(
                                "step '{}': {what} '{target}' has no recorded access level (the log was altered, or a pre-1.0 run is being resumed without its flow), so an access escalation cannot be ruled out - refusing to resume (set allow_access_override: true on this step to accept; if you edited the flow, --force-resume is needed too)",
                                step.id
                            ));
                        }
                    }
                    _ => {}
                }
                let new_cwd = cwd.as_ref().map(|c| c.display().to_string());
                let cwd_scoped = if is_fork {
                    preset::fork_is_cwd_scoped(&tool)
                } else {
                    preset::session_is_cwd_scoped(&tool)
                };
                if cwd_scoped && info.cwd != new_cwd {
                    // For tools that silently create on a lookup miss, a cwd
                    // change is not a risk to warn about - it guarantees a cold
                    // session wearing the right id.
                    if !is_fork && preset::resume_requires_same_cwd(&tool) {
                        return Err(format!(
                            "step '{}': {tool} looks sessions up per directory, and a miss silently starts a NEW chat; '{target}' ran in {:?} but this step uses {:?}",
                            step.id, info.cwd, new_cwd
                        ));
                    }
                    eprintln!(
                        "sfh: warning: step '{}': {tool} sessions are cwd-scoped; original ran in {:?}, this step uses {:?} - the session may not be found",
                        step.id, info.cwd, new_cwd
                    );
                }
                // cursor's marker is the chat store path: if it is gone, the
                // resume would quietly become a fresh chat.
                if !is_fork && tool == "cursor" {
                    match info.marker.as_deref() {
                        Some(p) if std::path::Path::new(p).is_file() => {}
                        Some(p) => {
                            return Err(format!(
                                "step '{}': cursor chat store for '{target}' is gone ({p}); resuming would silently start a NEW chat",
                                step.id
                            ))
                        }
                        None => {
                            return Err(format!(
                                "step '{}': cursor chat store for '{target}' was never located, so a resume cannot be verified",
                                step.id
                            ))
                        }
                    }
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
    let (
        parse,
        delivery,
        preassigned,
        expect_session,
        expect_marker,
        mut env_remove,
        preset_env_set,
    ): (
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
    // Flow/profile/default env first, then the preset's own env LAST. execute.rs
    // applies env_set last-wins, so this ordering makes the preset's access
    // enforcement (opencode's OPENCODE_CONFIG_CONTENT bash/edit/external-dir
    // deny) override a same-named flow env that would otherwise re-allow bash and
    // turn a read/write step into full access. The enforcement must not be
    // overridable by flow data, which is template-expanded and thus reachable
    // from run output (rev_break #10).
    let mut env_set: Vec<(String, String)> = Vec::new();
    for (k, v) in &eff.env {
        env_set.push((k.clone(), rend("env", v)?));
    }
    env_set.extend(preset_env_set);

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
        run_dir: cx.run_dir.to_path_buf(),
        out_file,
        err_file,
        chain_file,
        access: tool_used.as_ref().map(|_| eff.access),
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

/// What a resumed or forked run has to prove about the session it landed in.
pub struct SessionExpect<'a> {
    pub expect_session: Option<&'a str>,
    pub expect_marker: Option<&'a str>,
    pub forbid_session: Option<&'a str>,
    pub expect_parent: Option<&'a str>,
    pub allow_empty: bool,
}

/// The session state machine, as a pure decision: `Some(reason)` means the run
/// must be treated as failed even though the tool exited 0.
///
/// Every tool here will happily report success while doing the wrong thing -
/// silently opening a NEW session on a resume, or ignoring a fork flag and
/// appending to the parent that the siblings are also writing to. Those are
/// unrecoverable in-band lies, so they get checked, and they get checked here
/// rather than inline so each branch can be tested without spawning anything.
pub fn check_session(e: &SessionExpect, parsed: &ParsedOut, chain: &str) -> Option<String> {
    let mismatch = |what: &str, exp: &str, got: &str| {
        Some(format!(
            "\nsfh: resume mismatch: expected {what} '{exp}' but the tool reported '{got}' (it silently started a new session - resuming from a different working directory does this)\n"
        ))
    };
    // A resume asks the tool to continue a specific session. Every supported
    // tool echoes the session id back on success, so a tool that silently starts
    // a FRESH session (or lands in a different one) reports a DIFFERENT id and is
    // caught here. A tool that reports NO id at all cannot be verified to have
    // resumed anything, so that is a failure too: the old (Some, None) fallback
    // accepted "the tool said nothing" as "it resumed the right session", which
    // let a CLI that ignored the resume flag pass as success (rev_break #16).
    // Fresh runs are unaffected: expect_session is only set on resume/fork.
    if let Some(exp) = e.expect_session {
        match parsed.session.as_deref() {
            Some(got) if got == exp => {}
            Some(got) => return mismatch("session", exp, got),
            None => {
                return Some(format!(
                    "\nsfh: resume unverified: the tool reported no session id, so sfh cannot tell whether it continued '{exp}' or silently started a fresh session\n"
                ))
            }
        }
    }
    // pi accepts any --session-id and creates one when it is not found in this
    // cwd, so the id matching proves nothing; the marker does. A missing marker
    // is as unverifiable as a missing id (rev_break #16).
    if let Some(exp) = e.expect_marker {
        match parsed.session_marker.as_deref() {
            Some(got) if got == exp => {}
            Some(got) => return mismatch("session marker", exp, got),
            None => {
                return Some(format!(
                    "\nsfh: resume unverified: the tool reported no session marker, so sfh cannot tell whether it found the session with marker '{exp}' or created a new one\n"
                ))
            }
        }
    }
    // A fork that came back as the parent means the fork flag was ignored: this
    // run appended to the session its siblings are also using. No reported id
    // means no proof the fork happened, which fails the same way (rev_break #16).
    if let Some(parent) = e.forbid_session {
        match parsed.session.as_deref() {
            Some(got) if got == parent => {
                return Some(format!(
                    "\nsfh: fork failed: the tool reported the PARENT session '{parent}' instead of a new one, so this run appended to the parent instead of branching\n"
                ))
            }
            Some(_) => {}
            None => {
                return Some(
                    "\nsfh: fork unverified: the tool reported no session id, so sfh cannot tell whether it branched the parent or appended to it\n".to_string(),
                )
            }
        }
    }
    // pi names the parent it branched from - positive proof of a fork.
    if let Some(parent) = e.expect_parent {
        let ok = parsed
            .session_parent
            .as_deref()
            .map(|sp| sp.contains(parent))
            .unwrap_or(false);
        if !ok {
            return Some(format!(
                "\nsfh: fork failed: the new session does not name '{parent}' as its parent (got {:?}), so it did not inherit the parent's context\n",
                parsed.session_parent
            ));
        }
    }
    if chain.trim().is_empty() && !e.allow_empty {
        return Some(
            "\nsfh: the tool exited successfully but produced no final message (set allow_empty: true if that is expected)\n".to_string(),
        );
    }
    None
}

/// Parse a tool's output. `run_dir`, when given, is the run dir the artifact
/// paths must stay inside: the codex --output-last-message file is written by
/// an external CLI and read back here, and on a resumed run that directory is
/// untrusted input, so the read is contained and no-follow (a symlink at the
/// fixed name used to pull external text into the chain output). A containment
/// violation fails the step instead of reading (rev_break #4). Callers without
/// a run dir (sfh doctor's own scratch dir, created and cleared by sfh) pass
/// None and get a plain read.
pub fn parse_output(
    parse: &preset::OutputParse,
    stdout: &str,
    stderr: &str,
    run_dir: Option<&Path>,
) -> Result<ParsedOut, String> {
    Ok(match parse {
        preset::OutputParse::Stdout => ParsedOut {
            text: stdout.trim().to_string(),
            ..Default::default()
        },
        preset::OutputParse::CodexJsonl(f) => {
            let mut o = parse_codex_jsonl(stdout);
            let file_text = match run_dir {
                Some(base) => contain::read_contained_abs(base, f)
                    .map(|t| t.unwrap_or_default())
                    .map_err(|e| format!("refusing to read the codex last-message file: {e}"))?,
                None => std::fs::read_to_string(f).unwrap_or_default(),
            };
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
    })
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
        let mut attempt_prep = prep.clone();
        if attempt > 0 {
            let number = attempt + 1;
            attempt_prep.tag = format!("{}.a{number}", prep.tag);
            attempt_prep.out_file = prep
                .out_file
                .with_file_name(format!("{}.a{number}.out.txt", prep.tag));
            attempt_prep.err_file = prep
                .err_file
                .with_file_name(format!("{}.a{number}.err.txt", prep.tag));
            attempt_prep.chain_file = prep
                .chain_file
                .with_file_name(format!("{}.a{number}.chain.txt", prep.tag));
        }
        let mut done = exec_once(attempt_prep);
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
    // The run dir is untrusted on a resumed run, and the artifact names are
    // predictable, so a symlink planted at <tag>.out.txt / .err.txt must fail
    // the step BEFORE anything writes through it: the stdout tee and (for
    // codex) an external CLI open these paths, and a link would redirect their
    // writes outside the run dir (rev_break #3). The codex last-message file
    // is handed to the CLI as a write target and read back by sfh, so it gets
    // the same refusal plus a no-follow pre-create: the CLI then overwrites a
    // regular file sfh just verified, not a link planted in between (rev_break
    // #4; the residual swap window is bounded by the 0700 run dir).
    let mut artifact_violation: Option<String> = None;
    for f in [&p.out_file, &p.err_file] {
        if f.symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            artifact_violation = Some(format!(
                "refusing to run: {} is a symlink; run artifacts must be regular files inside the run dir",
                f.display()
            ));
            break;
        }
    }
    if artifact_violation.is_none() {
        if let preset::OutputParse::CodexJsonl(last) = &p.parse {
            if last
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                artifact_violation = Some(format!(
                    "refusing to run: {} is a symlink; an external CLI would write through it out of the run dir",
                    last.display()
                ));
            } else if let Err(e) = contain::create_nofollow(last) {
                artifact_violation = Some(format!(
                    "refusing to run: cannot pre-create {} as a regular file: {e}",
                    last.display()
                ));
            }
        }
    }
    if let Some(why) = artifact_violation {
        // Only write the err file when it is itself safe (the violation may be
        // exactly "the err file is a symlink").
        if !p
            .err_file
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            let _ = contain::write_private(&p.err_file, &why);
        }
        if !p.quiet {
            eprintln!("sfh: [{}] {why}", p.tag);
        }
        return LeafDone {
            tag: p.tag,
            exit_code: -1,
            timed_out: false,
            interrupted: false,
            dur_ms: 0,
            attempts: 1,
            chain_output: String::new(),
            stderr_clean: why,
            out_file: p.out_file,
            session_id: None,
            session_marker: None,
            tool: p.tool,
            cwd: cwd_str,
            access: p.access,
            usage: preset::Usage::default(),
            cmd: cmd_desc,
        };
    }
    let outcome = match execute::run_cmd(
        &p.inv,
        p.stdin_payload,
        p.cwd.as_deref(),
        p.timeout,
        &p.env_remove,
        &p.env_set,
        // Tee to the step's out file so a long step is observable while it
        // runs; the cleaned text replaces it once the child exits.
        Some(p.out_file.clone()),
    ) {
        Ok(o) => o,
        Err(e) => {
            let _ = contain::write_private(&p.err_file, &e);
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
                access: p.access,
                usage: preset::Usage::default(),
                cmd: cmd_desc,
            };
        }
    };
    let stdout_clean = clean_text(&outcome.stdout);
    let mut stderr_clean = clean_text(&outcome.stderr);
    let _ = contain::write_private(&p.out_file, &stdout_clean);
    let _ = contain::write_private(&p.err_file, &stderr_clean);

    let parsed = match parse_output(&p.parse, &stdout_clean, &stderr_clean, Some(&p.run_dir)) {
        Ok(o) => o,
        Err(e) => {
            // A containment violation reading the tool's artifact is a failure
            // of this step, not empty output (rev_break #4).
            stderr_clean.push_str(&format!("\nsfh: {e}\n"));
            let _ = contain::write_private(&p.err_file, &stderr_clean);
            ParsedOut {
                failed: true,
                ..Default::default()
            }
        }
    };
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
    let chain_output = parsed.text.clone();

    let mut session_id = if exit_code == 0 && !outcome.timed_out {
        parsed
            .session
            .clone()
            .or_else(|| p.preassigned_session.clone())
    } else {
        None
    };
    // cursor has no in-band proof that a chat exists, so record where it landed
    // on disk; a later resume checks that path before spending anything.
    let mut session_marker = parsed.session_marker.clone();
    if p.tool.as_deref() == Some("cursor") {
        if let Some(id) = &session_id {
            session_marker = preset::cursor_chat_store(id).map(|p| p.display().to_string());
        }
    }
    if exit_code == 0 && !outcome.timed_out {
        let expect = SessionExpect {
            expect_session: p.expect_session.as_deref(),
            expect_marker: p.expect_marker.as_deref(),
            forbid_session: p.forbid_session.as_deref(),
            expect_parent: p.expect_parent.as_deref(),
            allow_empty: p.allow_empty,
        };
        if let Some(why) = check_session(&expect, &parsed, &chain_output) {
            exit_code = 1;
            if !why.starts_with("\nsfh: the tool exited successfully") {
                session_id = None;
            }
            stderr_clean.push_str(&why);
            let _ = contain::write_private(&p.err_file, &stderr_clean);
        }
    }
    let _ = contain::write_private(&p.chain_file, &chain_output);

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
        session_marker,
        tool: p.tool,
        cwd: cwd_str,
        access: p.access,
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
        access: None,
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

    fn expect_none() -> SessionExpect<'static> {
        SessionExpect {
            expect_session: None,
            expect_marker: None,
            forbid_session: None,
            expect_parent: None,
            allow_empty: false,
        }
    }

    fn parsed_with(session: Option<&str>, marker: Option<&str>, parent: Option<&str>) -> ParsedOut {
        ParsedOut {
            text: "answer".into(),
            session: session.map(String::from),
            session_marker: marker.map(String::from),
            session_parent: parent.map(String::from),
            ..Default::default()
        }
    }

    // Every one of these is a tool exiting 0 while having done the wrong thing.
    // They were all found against live CLIs; this keeps them found.
    #[test]
    fn a_resume_that_landed_in_a_different_session_is_a_failure() {
        let e = SessionExpect {
            expect_session: Some("sess-1"),
            ..expect_none()
        };
        let ok = check_session(&e, &parsed_with(Some("sess-1"), None, None), "answer");
        assert!(ok.is_none(), "matching id must pass: {ok:?}");

        let bad = check_session(&e, &parsed_with(Some("sess-2"), None, None), "answer")
            .expect("a different session id must fail");
        assert!(bad.contains("resume mismatch"), "{bad}");
        assert!(bad.contains("sess-1") && bad.contains("sess-2"), "{bad}");
    }

    #[test]
    fn a_marker_mismatch_fails_even_when_the_id_matches() {
        // pi echoes back whatever --session-id it was given, so the id proving
        // nothing is the entire reason the marker exists.
        let e = SessionExpect {
            expect_session: Some("same-id"),
            expect_marker: Some("2026-07-27T10:00:00Z"),
            ..expect_none()
        };
        let bad = check_session(
            &e,
            &parsed_with(Some("same-id"), Some("2026-07-28T09:00:00Z"), None),
            "answer",
        )
        .expect("a new creation timestamp means a new session");
        assert!(bad.contains("session marker"), "{bad}");
    }

    #[test]
    fn a_fork_that_returns_the_parent_session_is_a_failure() {
        let e = SessionExpect {
            forbid_session: Some("parent-1"),
            ..expect_none()
        };
        assert!(check_session(&e, &parsed_with(Some("child-9"), None, None), "answer").is_none());
        let bad = check_session(&e, &parsed_with(Some("parent-1"), None, None), "answer")
            .expect("appending to the parent must not look like success");
        assert!(
            bad.contains("fork failed") && bad.contains("PARENT"),
            "{bad}"
        );
    }

    #[test]
    fn a_fork_must_positively_name_its_parent_when_the_tool_reports_one() {
        let e = SessionExpect {
            expect_parent: Some("parent-1"),
            ..expect_none()
        };
        // Substring, because pi reports a path that contains the id.
        assert!(check_session(
            &e,
            &parsed_with(Some("c"), None, Some("/sessions/parent-1.jsonl")),
            "answer"
        )
        .is_none());
        let bad = check_session(&e, &parsed_with(Some("c"), None, None), "answer")
            .expect("no parent reported means no proof the context was inherited");
        assert!(bad.contains("did not inherit"), "{bad}");
    }

    #[test]
    fn an_empty_final_message_fails_unless_opted_in() {
        let e = expect_none();
        let bad = check_session(&e, &parsed_with(None, None, None), "   \n ")
            .expect("empty output must not flow into the next prompt");
        assert!(bad.contains("no final message"), "{bad}");
        let allowed = SessionExpect {
            allow_empty: true,
            ..expect_none()
        };
        assert!(check_session(&allowed, &parsed_with(None, None, None), "").is_none());
    }

    #[test]
    fn checks_do_not_fire_when_nothing_was_expected() {
        // A plain fresh run must never be failed by the session machinery.
        assert!(check_session(
            &expect_none(),
            &parsed_with(Some("whatever"), Some("m"), Some("p")),
            "answer"
        )
        .is_none());
    }

    fn temp_run_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("sfh-leaf-test-{}", gen_uuid()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn no_tainted_vars() -> &'static HashSet<String> {
        use std::sync::OnceLock;
        static EMPTY: OnceLock<HashSet<String>> = OnceLock::new();
        EMPTY.get_or_init(HashSet::new)
    }

    #[allow(clippy::too_many_arguments)]
    fn ctx<'a>(
        flow: &'a flow::Flow,
        vars: &'a BTreeMap<String, String>,
        outputs: &'a BTreeMap<String, template::StepOutput>,
        step_ids: &'a HashSet<String>,
        run_dir: &'a Path,
        sessions: &'a HashMap<String, SessionInfo>,
        needed: &'a HashSet<String>,
    ) -> PrepCtx<'a> {
        PrepCtx {
            flow,
            vars,
            outputs,
            step_ids,
            run_dir,
            flow_dir: run_dir,
            notes_file: run_dir,
            sessions,
            needed_sessions: needed,
            tainted_vars: no_tainted_vars(),
            quiet: true,
            verbose: false,
        }
    }

    // A session ingested at low access may carry untrusted content; resuming it
    // at a higher tier is the classic promotion path and must fail closed.
    #[test]
    fn resume_cannot_escalate_the_sessions_access_level() {
        let flow: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: low\n    tool: claude\n    access: read\n    prompt: x\n  - id: high\n    tool: claude\n    access: full\n    continue_from: low\n    prompt: y\n",
        )
        .unwrap();
        let dir = temp_run_dir();
        let vars = BTreeMap::new();
        let outputs = BTreeMap::new();
        let step_ids = flow.step_ids();
        let needed: HashSet<String> = ["low".into()].into_iter().collect();
        let with_access = |a: Option<preset::Access>| {
            let mut sessions = HashMap::new();
            sessions.insert(
                "low".to_string(),
                SessionInfo {
                    tool: "claude".into(),
                    id: "sess-1".into(),
                    cwd: None,
                    marker: None,
                    access: a,
                    // This case is "the level is unknown at the guard", which
                    // is what the guard is being tested on; how it got that
                    // way is reconcile_session_access's business, not this
                    // function's.
                    access_recorded: a.is_some(),
                },
            );
            sessions
        };
        let high = &flow.steps[1];

        let sessions = with_access(Some(preset::Access::Read));
        let e = prepare_leaf(
            &ctx(&flow, &vars, &outputs, &step_ids, &dir, &sessions, &needed),
            high,
            1,
            "high",
            &[],
            None,
        )
        .err()
        .expect("read -> full resume must be refused");
        assert!(e.contains("higher access level"), "{e}");
        assert!(e.contains("read") && e.contains("full"), "{e}");

        // the explicit opt-in is the only way through
        let mut opted: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: low\n    tool: claude\n    access: read\n    prompt: x\n  - id: high\n    tool: claude\n    access: full\n    allow_access_override: true\n    continue_from: low\n    prompt: y\n",
        )
        .unwrap();
        let high_o = opted.steps.pop().unwrap();
        prepare_leaf(
            &ctx(&flow, &vars, &outputs, &step_ids, &dir, &sessions, &needed),
            &high_o,
            1,
            "high",
            &[],
            None,
        )
        .expect("allow_access_override permits the escalation");

        // same tier and downgrades are fine
        let mut same: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: low\n    tool: claude\n    access: read\n    prompt: x\n  - id: high\n    tool: claude\n    access: read\n    continue_from: low\n    prompt: y\n",
        )
        .unwrap();
        let high_s = same.steps.pop().unwrap();
        prepare_leaf(
            &ctx(&flow, &vars, &outputs, &step_ids, &dir, &sessions, &needed),
            &high_s,
            1,
            "high",
            &[],
            None,
        )
        .expect("resuming at the same access level is allowed");

        // write -> full is an escalation too
        let sessions_w = with_access(Some(preset::Access::Write));
        assert!(prepare_leaf(
            &ctx(
                &flow,
                &vars,
                &outputs,
                &step_ids,
                &dir,
                &sessions_w,
                &needed
            ),
            high,
            1,
            "high",
            &[],
            None,
        )
        .is_err());

        // A session with no recorded level (old run dir, or an edited log) is
        // fail-CLOSED: an attacker who controls the run dir could otherwise
        // delete the field and resume a read session at full.
        let sessions_unknown = with_access(None);
        let e = prepare_leaf(
            &ctx(
                &flow,
                &vars,
                &outputs,
                &step_ids,
                &dir,
                &sessions_unknown,
                &needed,
            ),
            high,
            1,
            "high",
            &[],
            None,
        )
        .err()
        .expect("an unknown recorded access level must fail closed");
        assert!(e.contains("no recorded access level"), "{e}");
        assert!(e.contains("allow_access_override"), "{e}");

        // the explicit opt-in downgrades the refusal to a warning
        let mut opted_unknown: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: low\n    tool: claude\n    access: read\n    prompt: x\n  - id: high\n    tool: claude\n    access: full\n    allow_access_override: true\n    continue_from: low\n    prompt: y\n",
        )
        .unwrap();
        let high_uo = opted_unknown.steps.pop().unwrap();
        prepare_leaf(
            &ctx(
                &flow,
                &vars,
                &outputs,
                &step_ids,
                &dir,
                &sessions_unknown,
                &needed,
            ),
            &high_uo,
            1,
            "high",
            &[],
            None,
        )
        .expect("allow_access_override accepts an unknown recorded level");

        // rev_complete S2-4: a missing recorded level is fail-CLOSED at EVERY
        // tier, read included. "read is the lowest tier so nothing can escalate
        // into it" is true for an honest missing field but indistinguishable
        // from one an attacker deleted, so the opt-in is required either way.
        // Legitimate pre-1.0 runs are filled from the flow by the engine
        // (reconcile_session_access) before this guard runs, so they never hit
        // this path.
        let mut read_resume: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: low\n    tool: claude\n    access: read\n    prompt: x\n  - id: again\n    tool: claude\n    access: read\n    continue_from: low\n    prompt: y\n",
        )
        .unwrap();
        let again = read_resume.steps.pop().unwrap();
        let e = prepare_leaf(
            &ctx(
                &read_resume,
                &vars,
                &outputs,
                &step_ids,
                &dir,
                &sessions_unknown,
                &needed,
            ),
            &again,
            1,
            "again",
            &[],
            None,
        )
        .err()
        .expect("an unknown recorded level must fail closed even at read");
        assert!(e.contains("no recorded access level"), "{e}");

        // ...and allow_access_override is the explicit way back in at read too.
        let mut read_resume_opt: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: low\n    tool: claude\n    access: read\n    prompt: x\n  - id: again\n    tool: claude\n    access: read\n    allow_access_override: true\n    continue_from: low\n    prompt: y\n",
        )
        .unwrap();
        let again_opt = read_resume_opt.steps.pop().unwrap();
        prepare_leaf(
            &ctx(
                &read_resume_opt,
                &vars,
                &outputs,
                &step_ids,
                &dir,
                &sessions_unknown,
                &needed,
            ),
            &again_opt,
            1,
            "again",
            &[],
            None,
        )
        .expect("allow_access_override accepts an unknown level at read");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // args may contain templates, so the escalation check must run again on the
    // RENDERED args: an upstream output can inject a permission flag that no
    // load-time check ever saw.
    #[test]
    fn rendered_args_are_checked_for_permission_flags() {
        let flow: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: a\n    tool: claude\n    access: read\n    args: [\"{{vars.flag}}\"]\n    prompt: x\n",
        )
        .unwrap();
        let dir = temp_run_dir();
        let mut vars = BTreeMap::new();
        vars.insert("flag".to_string(), "--force".to_string());
        let outputs = BTreeMap::new();
        let step_ids = flow.step_ids();
        let sessions = HashMap::new();
        let needed = HashSet::new();
        let e = prepare_leaf(
            &ctx(&flow, &vars, &outputs, &step_ids, &dir, &sessions, &needed),
            &flow.steps[0],
            1,
            "a",
            &[],
            None,
        )
        .err()
        .expect("a rendered --force arg must be refused at read access");
        assert!(e.contains("overrides the declared access level"), "{e}");
        assert!(e.contains("--force"), "{e}");

        // a benign value passes the same path
        vars.insert("flag".to_string(), "--verbose".to_string());
        prepare_leaf(
            &ctx(&flow, &vars, &outputs, &step_ids, &dir, &sessions, &needed),
            &flow.steps[0],
            1,
            "a",
            &[],
            None,
        )
        .expect("a non-permission flag must pass");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // The metacharacter blacklist stops shell DELIMITERS, not hostile VALUES:
    // the audit's `--checkpoint-action=exec='sh payload.sh'` contains no banned
    // character at all, yet runs arbitrary code through tar. So expansion in a
    // string cmd is refused outright unless the step opts in.
    #[test]
    fn string_cmd_template_expansion_is_disabled_by_default() {
        let flow: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: a\n    cmd: \"tar -cf backup.tar {{vars.files}}\"\n",
        )
        .unwrap();
        let dir = temp_run_dir();
        let mut vars = BTreeMap::new();
        vars.insert(
            "files".to_string(),
            "--checkpoint=1 --checkpoint-action=exec='sh payload.sh' harmless.txt".to_string(),
        );
        let outputs = BTreeMap::new();
        let step_ids = flow.step_ids();
        let sessions = HashMap::new();
        let needed = HashSet::new();
        let e = prepare_leaf(
            &ctx(&flow, &vars, &outputs, &step_ids, &dir, &sessions, &needed),
            &flow.steps[0],
            1,
            "a",
            &[],
            None,
        )
        .err()
        .expect("expansion in a string cmd must be refused by default");
        assert!(e.contains("disabled by default"), "{e}");
        assert!(e.contains("cmd: ["), "{e}");

        // The same value through the argv form is data, not shell syntax.
        let argv: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: a\n    cmd: [\"printf\", \"%s\\n\", \"{{vars.files}}\"]\n",
        )
        .unwrap();
        let p = prepare_leaf(
            &ctx(&flow, &vars, &outputs, &step_ids, &dir, &sessions, &needed),
            &argv.steps[0],
            1,
            "a",
            &[],
            None,
        )
        .expect("the argv form may carry any value as one argument");
        match p.inv {
            execute::Invocation::Argv(v) => {
                assert_eq!(
                    v.last().map(String::as_str),
                    Some("--checkpoint=1 --checkpoint-action=exec='sh payload.sh' harmless.txt")
                );
            }
            _ => panic!("argv form must spawn directly"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsafe_shell_template_opts_back_into_shell_templating() {
        let dir = temp_run_dir();
        let mut vars = BTreeMap::new();
        vars.insert("f".to_string(), "harmless.txt".to_string());
        let outputs = BTreeMap::new();
        let sessions = HashMap::new();
        let needed = HashSet::new();

        // Opted in, benign value: expands as before.
        let ok: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: a\n    unsafe_shell_template: true\n    cmd: \"echo {{vars.f}}\"\n",
        )
        .unwrap();
        let step_ids = ok.step_ids();
        let p = prepare_leaf(
            &ctx(&ok, &vars, &outputs, &step_ids, &dir, &sessions, &needed),
            &ok.steps[0],
            1,
            "a",
            &[],
            None,
        )
        .expect("opt-in allows a benign expansion");
        assert!(matches!(p.inv, execute::Invocation::Shell(_)));

        // Opted in, metacharacters: the delimiter check still fires.
        vars.insert("f".to_string(), "x & echo pwned".to_string());
        let e = prepare_leaf(
            &ctx(&ok, &vars, &outputs, &step_ids, &dir, &sessions, &needed),
            &ok.steps[0],
            1,
            "a",
            &[],
            None,
        )
        .err()
        .expect("metacharacters are still rejected under the opt-in");
        assert!(e.contains("metacharacters"), "{e}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_script_span_detects_wrapped_shells() {
        let s =
            |a: &[&str]| shell_script_span(&a.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        // rev_break #13: the argv form can wrap a shell; the script argument
        // after the -c / /C flag is re-parsed by that shell.
        assert_eq!(s(&["sh", "-c", "echo hi"]), Some(2..3));
        assert_eq!(s(&["bash", "-c", "x"]), Some(2..3));
        // Flags before -c: the script is still the element after -c.
        assert_eq!(s(&["sh", "-l", "-c", "x"]), Some(3..4));

        // sh -c SCRIPT name arg1: only SCRIPT is shell text. The trailing
        // arguments arrive as $0/$1 inside the script and are NOT re-parsed -
        // this is the recommended way to pass an untrusted value to a shell,
        // and a span that swallowed the tail refused it.
        assert_eq!(
            s(&["sh", "-c", "cat \"$1\"", "name", "/some/path"]),
            Some(2..3)
        );

        // cmd.exe and PowerShell -Command re-join everything that follows into
        // one command line, so the whole tail is shell text.
        assert_eq!(s(&["cmd", "/C", "dir"]), Some(2..3));
        assert_eq!(s(&["cmd", "/C", "echo", "a", "b"]), Some(2..5));
        assert_eq!(s(&["powershell", "-Command", "echo", "a"]), Some(2..4));
        assert_eq!(s(&["pwsh", "-c", "echo", "a"]), Some(2..4));
        assert_eq!(s(&["pwsh", "-NoProfile", "-Command", "x"]), Some(3..4));
        // -EncodedCommand takes exactly one base64 argument.
        assert_eq!(s(&["pwsh", "-EncodedCommand", "eABiAA==", "x"]), Some(2..3));
        // Either introducer, and a path- or extension-qualified name, on every
        // OS: this is parsed by hand rather than with Path so that a Windows
        // path is still recognised when the check runs on Linux.
        assert_eq!(s(&["pwsh", "/Command", "x"]), Some(2..3));
        assert_eq!(s(&["/bin/sh", "-c", "x"]), Some(2..3));
        assert_eq!(
            s(&["C:\\Windows\\System32\\cmd.exe", "/c", "x"]),
            Some(2..3)
        );
        assert_eq!(s(&["/usr/bin/pwsh", "-Command", "x"]), Some(2..3));

        // PowerShell switches that merely LOOK close must not be mistaken for
        // -Command or -EncodedCommand, or their operand would be refused.
        assert_eq!(s(&["pwsh", "-ExecutionPolicy", "Bypass"]), None);
        assert_eq!(s(&["pwsh", "-ConfigurationName", "n"]), None);
        assert_eq!(s(&["pwsh", "-NoProfile", "-File", "s.ps1"]), None);

        // Not a shell, or no run-string flag: no shell text.
        assert_eq!(s(&["echo", "hi"]), None);
        assert_eq!(s(&["sh", "script.sh"]), None);
        // A script merely NAMED after a shell is not one. Only ".exe" comes
        // off, so sh.py stays sh.py.
        assert_eq!(s(&["sh.py", "-c", "x"]), None);
        assert_eq!(s(&["bash.sh", "-c", "x"]), None);
        assert_eq!(s(&["/opt/tools/cmd.pl", "/c", "x"]), None);
        assert_eq!(shell_script_span(&[]), None);
        // A flag with nothing after it must not produce an out-of-range span.
        assert_eq!(s(&["sh", "-c"]), Some(2..2));
    }

    #[test]
    fn argv_wrapped_shell_applies_shell_template_rules() {
        // rev_break #13: cmd: ["sh","-c","...{{x}}..."] is the argv branch, but
        // its third argument is shell text, so it must hit the same refusal as a
        // string-form cmd - the old code skipped every shell defence here.
        let dir = temp_run_dir();
        let mut vars = BTreeMap::new();
        vars.insert("u".to_string(), "value".to_string());
        let outputs = BTreeMap::new();
        let sessions = HashMap::new();
        let needed = HashSet::new();

        let f: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: a\n    cmd: [\"sh\", \"-c\", \"echo {{vars.u}}\"]\n",
        )
        .unwrap();
        let step_ids = f.step_ids();
        let e = prepare_leaf(
            &ctx(&f, &vars, &outputs, &step_ids, &dir, &sessions, &needed),
            &f.steps[0],
            1,
            "a",
            &[],
            None,
        )
        .err()
        .expect("shell text in an argv-wrapped shell must be refused by default");
        assert!(e.contains("disabled by default"), "{e}");

        // Non-shell arguments (before the script, or in a non-wrapping argv) are
        // plain data and expand freely.
        let g: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: a\n    cmd: [\"printf\", \"%s\", \"{{vars.u}}\"]\n",
        )
        .unwrap();
        let gids = g.step_ids();
        let p = prepare_leaf(
            &ctx(&g, &vars, &outputs, &gids, &dir, &sessions, &needed),
            &g.steps[0],
            1,
            "a",
            &[],
            None,
        )
        .expect("a non-wrapping argv expands its data arguments");
        match p.inv {
            execute::Invocation::Argv(v) => assert_eq!(v.last().map(String::as_str), Some("value")),
            _ => panic!("argv form must spawn directly"),
        }

        // The opt-in allows it, with the metacharacter check still live.
        let h: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: a\n    unsafe_shell_template: true\n    cmd: [\"sh\", \"-c\", \"echo {{vars.u}}\"]\n",
        )
        .unwrap();
        let hids = h.step_ids();
        prepare_leaf(
            &ctx(&h, &vars, &outputs, &hids, &dir, &sessions, &needed),
            &h.steps[0],
            1,
            "a",
            &[],
            None,
        )
        .expect("unsafe_shell_template allows a benign argv-wrapped expansion");
        vars.insert("u".to_string(), "a & b".to_string());
        let e = prepare_leaf(
            &ctx(&h, &vars, &outputs, &hids, &dir, &sessions, &needed),
            &h.steps[0],
            1,
            "a",
            &[],
            None,
        )
        .err()
        .expect("metacharacters are still rejected under the opt-in");
        assert!(e.contains("metacharacters"), "{e}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exec_privileged_fields_refuse_run_derived_templates() {
        // rev_break #12: bin / cwd / argv[0] are executed with sfh's own rights,
        // so step output (and notes / foreach item) may not flow into them.
        let dir = temp_run_dir();
        let vars = BTreeMap::new();
        let outputs = BTreeMap::new();
        let sessions = HashMap::new();
        let needed = HashSet::new();

        // bin from step output is refused (step 'a' exists so the template
        // resolves; the exec-path check then rejects the run-derived value).
        let f: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: a\n    cmd: [\"echo\"]\n  - id: b\n    cmd: [\"echo\"]\n    bin: \"{{steps.a.output}}\"\n",
        )
        .unwrap();
        let step_ids = f.step_ids();
        let e = prepare_leaf(
            &ctx(&f, &vars, &outputs, &step_ids, &dir, &sessions, &needed),
            &f.steps[1],
            1,
            "b",
            &[],
            None,
        )
        .err()
        .expect("bin from step output must be refused");
        assert!(e.contains("executed by sfh"), "{e}");
        assert!(e.contains("allow_dynamic_exec_paths"), "{e}");

        // argv[0] from step output is refused.
        let g: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: a\n    cmd: [\"echo\"]\n  - id: b\n    cmd: [\"{{steps.a.output}}\", \"--flag\"]\n",
        )
        .unwrap();
        let gids = g.step_ids();
        let e = prepare_leaf(
            &ctx(&g, &vars, &outputs, &gids, &dir, &sessions, &needed),
            &g.steps[1],
            1,
            "b",
            &[],
            None,
        )
        .err()
        .expect("argv[0] from step output must be refused");
        assert!(e.contains("executed by sfh"), "{e}");

        // The escape hatch lets run-derived values through.
        let h: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: a\n    cmd: [\"echo\"]\n  - id: b\n    allow_dynamic_exec_paths: true\n    cmd: [\"echo\"]\n    cwd: \"{{steps.a.output}}\"\n",
        )
        .unwrap();
        let hids = h.step_ids();
        prepare_leaf(
            &ctx(&h, &vars, &outputs, &hids, &dir, &sessions, &needed),
            &h.steps[1],
            1,
            "b",
            &[],
            None,
        )
        .expect("allow_dynamic_exec_paths accepts run-derived cwd");

        // A user var (not tainted) is still allowed in cwd without the hatch.
        let mut vars2 = BTreeMap::new();
        vars2.insert("base".to_string(), "/work".to_string());
        let i: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: a\n    cmd: [\"echo\"]\n    cwd: \"{{vars.base}}\"\n",
        )
        .unwrap();
        let iids = i.step_ids();
        prepare_leaf(
            &ctx(&i, &vars2, &outputs, &iids, &dir, &sessions, &needed),
            &i.steps[0],
            1,
            "a",
            &[],
            None,
        )
        .expect("a user-controlled var is allowed in cwd");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tainted_var_is_refused_in_exec_fields() {
        // rev_break #12: a var restored from the resumed run dir's meta.json is
        // untrusted; the same {{vars.x}} a user typed themselves is fine.
        let dir = temp_run_dir();
        let mut vars = BTreeMap::new();
        vars.insert("base".to_string(), "/work".to_string());
        let mut tainted = HashSet::new();
        tainted.insert("base".to_string());
        let outputs = BTreeMap::new();
        let step_ids: HashSet<String> = ["a".into()].into_iter().collect();
        let sessions = HashMap::new();
        let needed = HashSet::new();

        let f: flow::Flow = serde_yaml_ng::from_str(
            "steps:\n  - id: a\n    cmd: [\"echo\"]\n    cwd: \"{{vars.base}}\"\n",
        )
        .unwrap();
        let cx = PrepCtx {
            flow: &f,
            vars: &vars,
            outputs: &outputs,
            step_ids: &step_ids,
            run_dir: &dir,
            flow_dir: &dir,
            notes_file: &dir,
            sessions: &sessions,
            needed_sessions: &needed,
            tainted_vars: &tainted,
            quiet: true,
            verbose: false,
        };
        let e = prepare_leaf(&cx, &f.steps[0], 1, "a", &[], None)
            .err()
            .expect("a tainted var in cwd must be refused");
        assert!(e.contains("meta.json"), "{e}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_session_fails_closed_when_the_tool_reports_nothing() {
        // rev_break #16: a resume/fork the tool does not confirm is a failure,
        // not a silent success on the preassigned/expected id.
        let resume = SessionExpect {
            expect_session: Some("sess-1"),
            expect_marker: None,
            forbid_session: None,
            expect_parent: None,
            allow_empty: true,
        };
        let bad = check_session(&resume, &parsed_with(None, None, None), "answer")
            .expect("no reported session id on a resume must fail");
        assert!(bad.contains("resume unverified"), "{bad}");

        let marker = SessionExpect {
            expect_session: Some("same"),
            expect_marker: Some("2026-07-27T10:00:00Z"),
            forbid_session: None,
            expect_parent: None,
            allow_empty: true,
        };
        let bad = check_session(&marker, &parsed_with(Some("same"), None, None), "answer")
            .expect("no reported marker on a resume must fail");
        assert!(bad.contains("session marker"), "{bad}");

        let fork = SessionExpect {
            expect_session: None,
            expect_marker: None,
            forbid_session: Some("parent-1"),
            expect_parent: None,
            allow_empty: true,
        };
        let bad = check_session(&fork, &parsed_with(None, None, None), "answer")
            .expect("no reported session id on a fork must fail");
        assert!(bad.contains("fork unverified"), "{bad}");
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
