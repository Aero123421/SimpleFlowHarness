use crate::execute::OutputObserver;
use crate::protocol::{self, ProtocolEvidence, ProtocolState};
use crate::{contain, execute, flow, preset, template};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex};
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
    /// Silence, in seconds, that makes a timeout count as a hang rather than as
    /// honest overrun. Only `RetryMode::Transient` consults it.
    pub hang_after_sec: u64,
}

/// Default silence before a timeout is read as a hang (seconds).
pub const DEFAULT_HANG_AFTER_SEC: u64 = 300;

impl Default for RetryCfg {
    fn default() -> Self {
        RetryCfg {
            max: 0,
            backoff_sec: 5,
            mode: RetryMode::Transient,
            hang_after_sec: DEFAULT_HANG_AFTER_SEC,
        }
    }
}

/// The session this step was resolved to continue or fork, as decided while
/// preparing it. Recorded in step_start so a reader of log.jsonl can follow the
/// session lineage without re-deriving it from the flow (which, after an edit
/// plus --force-resume, no longer describes what actually ran).
#[derive(Clone)]
pub struct SessionParent {
    /// "continue" (same session) or "fork" (a branch of it).
    pub mode: &'static str,
    /// The step whose session this one attached to.
    pub step: String,
    pub tool: String,
    /// The PARENT's session id. A fork's own child id is minted inside the
    /// preset builder and is reported back by the tool, so it lands in step_end.
    pub id: String,
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
    /// Absolute run-level deadline. Unlike a relative step timeout this also
    /// expires while a leaf waits in a bounded fan-out queue.
    pub wall_deadline: Option<std::time::Instant>,
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
    /// Set when continue_from / fork_from resolved to a recorded session.
    pub session_parent: Option<SessionParent>,
    pub allow_empty: bool,
    /// Hash of the assembled context bundle, or None when the step named none.
    /// Recorded in `step_start` so a reader can tell two visits apart by what
    /// they were handed, without the log carrying the content itself.
    pub context_hash: Option<String>,
    pub context_file: Option<PathBuf>,
    /// What the flow declared about exit-code/protocol disagreement, or None to
    /// keep the adapter's own default.
    pub exit_conflict: Option<flow::ExitConflict>,
    /// This step's `outcomes:` table, keyed by raw process exit code.
    pub outcomes: BTreeMap<i32, flow::Outcome>,
    pub retry: RetryCfg,
    /// Run-level activity clock every child of this run touches when it writes
    /// anything, so `status.json` can say how long the whole run has been quiet.
    pub run_clock: Option<Arc<std::sync::atomic::AtomicU64>>,
    pub quiet: bool,
    pub verbose: bool,
}

pub struct LeafDone {
    pub tag: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub interrupted: bool,
    pub dur_ms: u128,
    /// Silence before the child exited or was killed (see ExecOutcome::idle_ms).
    pub idle_ms: u64,
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
    /// Bounded sfh-generated diagnosis for machine-readable `runs why` output.
    /// Tool-controlled stderr is never copied here.
    pub harness_diagnostic: Option<String>,
    /// A required run artifact could not be persisted. This is not an ordinary
    /// tool failure that on_error/fallback may ignore: without the artifact a
    /// later resume cannot reconstruct what happened.
    pub persistence_error: Option<String>,
    /// The `outcomes:` entry that matched this step's raw process exit, if any.
    /// `None` means the flow declared nothing for that code and the historical
    /// reading stands.
    pub outcome: Option<(flow::OutcomeResult, Option<String>)>,
    /// What the structured protocol proved, recorded in `step_end` so a reader
    /// can tell "the tool failed" from "sfh could not verify that it finished".
    pub protocol: ProtocolEvidence,
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
    let hang_after_sec = step
        .hang_after_sec
        .or(flow.defaults.hang_after_sec)
        .unwrap_or(DEFAULT_HANG_AFTER_SEC);
    match r {
        Some(r) => RetryCfg {
            max: r.max,
            backoff_sec: r.backoff_sec.unwrap_or(5),
            mode,
            hang_after_sec,
        },
        None => RetryCfg {
            max: 0,
            backoff_sec: 5,
            mode,
            hang_after_sec,
        },
    }
}

/// What `{{budget.*}}` renders to for one step. Snapshot values, taken when the
/// step is prepared: a prompt that says "you have $2 left" has to mean the
/// moment the prompt was written, not some later moment.
///
/// `remaining_*` is None when that axis has no ceiling, which renders as the
/// string `unlimited` rather than as an empty value or a made-up number - a
/// prompt reading "0 seconds left" when nothing is capped would be a lie.
#[derive(Clone, Copy, Default)]
pub struct BudgetVars {
    pub spent_usd: f64,
    pub elapsed_sec: u64,
    pub remaining_usd: Option<f64>,
    pub remaining_sec: Option<u64>,
}

impl BudgetVars {
    /// `spent_usd` is reported cost so far (restored cost included on a
    /// resume); `elapsed_sec` is measured from the start of THIS process's flow
    /// loop, which is the same clock `wall_clock_sec` is judged on.
    ///
    /// Both remainders are measured against the CEILING, not against the
    /// on_budget threshold: the reserve is headroom for the landing chain, and
    /// a landing step asking "how much is left" means how much is really left.
    /// They clamp at zero, because "how much budget remains" cannot be a
    /// negative quantity even when a single step overshot the ceiling.
    pub fn new(defaults: &flow::Defaults, spent_usd: f64, elapsed_sec: u64) -> Self {
        Self {
            spent_usd,
            elapsed_sec,
            remaining_usd: defaults.max_cost_usd.map(|m| (m - spent_usd).max(0.0)),
            remaining_sec: defaults
                .wall_clock_sec
                .map(|s| s.saturating_sub(elapsed_sec)),
        }
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
    /// Run-level activity clock handed to every leaf this context prepares.
    pub run_clock: Option<&'a Arc<std::sync::atomic::AtomicU64>>,
    /// Hard run deadline applied to every prepared leaf.
    pub wall_deadline: Option<std::time::Instant>,
    /// Spend and time as of now, exposed to templates as `{{budget.*}}`.
    pub budget: BudgetVars,
    /// The managed workspace this run's steps execute in, when the flow asked
    /// for one. A step's own `cwd:` still wins - it is a deliberate statement
    /// about where THAT step belongs - but everything else runs here instead of
    /// in the caller's directory.
    pub workspace: Option<&'a Path>,
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

pub fn exec_template_check(
    tainted_vars: &HashSet<String>,
) -> impl Fn(&str, &str) -> Result<(), String> + '_ {
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
        // The SWITCH itself can be a template. This runs before rendering -
        // that is the point, the answer has to be known before anything is
        // spent - so an argument still holding {{...}} could turn into
        // -Command, -c, /C or anything else at run time:
        //   cmd: ["pwsh", "{{steps.mode.output}}", "Write-Output {{x}}"]
        // read as a bare word here, and the third argument then rendered as
        // ordinary data straight into PowerShell. Once the classification
        // depends on a value that does not exist yet, the only honest answer
        // is that everything from here on might be shell text.
        if template::contains_template(a) {
            return Some(i..argv.len());
        }
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
        //
        // A reviewer later argued that pwsh 7.4 accepts only the exact
        // -CommandWithArgs and -cwa, and that the prefixes should therefore be
        // dropped. Kept, because the two errors are not symmetric: if pwsh does
        // take -CommandW, failing to recognise it lets the script text through
        // unchecked; if it does not, the flow was never going to run, so
        // refusing it costs a broken flow a clear error instead of a confusing
        // one. Over-detection here has no legitimate use to block.
        if powershell {
            // Everything after -File is arguments TO the script, and so is
            // everything after the first bare word, which pwsh also reads as a
            // file. A -Command sitting there is data, not a switch, and
            // scanning past it refused
            //   ["pwsh","-File","s.ps1","-Command","{{x}}"]
            // where nothing is ever re-parsed as code.
            let is_switch = low.starts_with('-') || low.starts_with('/');
            if !is_switch {
                return None;
            }
            let bare_f = low.trim_start_matches(['-', '/']);
            if !bare_f.is_empty() && "file".starts_with(bare_f) {
                return None;
            }
        }
        if powershell && (low.starts_with('-') || low.starts_with('/')) {
            let bare = low.trim_start_matches(['-', '/']);
            if bare.is_empty() {
                continue;
            }
            // -CommandWithArgs (pwsh 7.4+) and its documented short form -cwa
            // take ONE command string; everything after it is handed to the
            // script as $args and is never re-parsed - the same shape as the
            // arguments after `sh -c SCRIPT`. Swallowing the tail here refused
            //   ["pwsh","-CommandWithArgs","Write-Output $args[0]","{{x}}"]
            // which is the safe way to write it.
            //
            // PowerShell takes any UNAMBIGUOUS prefix, so -CommandW and
            // -CommandWi already mean -CommandWithArgs. Matching only the exact
            // name and -cwa left those spellings hitting neither branch - not
            // over-rejected but UNDETECTED, with the script text passing as
            // ordinary argv data. Anything longer than "command" can only be
            // heading for the longer name; "command" itself and shorter is
            // -Command, which is how the shell resolves them too.
            if bare == "cwa"
                || (bare.len() > "command".len() && "commandwithargs".starts_with(bare))
            {
                return Some(i + 1..(i + 2).min(argv.len()));
            }
            // -EncodedCommand also takes exactly one argument, a base64 blob.
            // Checked before -Command because "e" is a prefix of neither name's
            // continuation but "c" is a prefix of "command" alone.
            if "encodedcommand".starts_with(bare) && bare.starts_with('e') {
                return Some(i + 1..(i + 2).min(argv.len()));
            }
            // -Command really does re-join everything that follows into one
            // command line, so there the whole tail is shell text.
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
    // Cost is printed to 4 decimals everywhere else in sfh (progress lines,
    // run_end, runs list), so a prompt that quotes it matches what the operator
    // sees. Seconds are whole, like every other duration in a flow file.
    b.insert(
        "budget.spent_usd".into(),
        format!("{:.4}", cx.budget.spent_usd),
    );
    b.insert(
        "budget.elapsed_sec".into(),
        cx.budget.elapsed_sec.to_string(),
    );
    b.insert(
        "budget.remaining_usd".into(),
        cx.budget
            .remaining_usd
            .map_or_else(|| "unlimited".to_string(), |v| format!("{v:.4}")),
    );
    b.insert(
        "budget.remaining_sec".into(),
        cx.budget
            .remaining_sec
            .map_or_else(|| "unlimited".to_string(), |v| v.to_string()),
    );
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
    let context_file = cx.run_dir.join(format!("{tag}.context.txt"));
    // The context bundle is assembled BEFORE the prompt, because `{{context}}`
    // and `{{context_file}}` are builtins a prompt may reference. A `template:`
    // context is rendered with the same variables a prompt sees, minus those
    // two - a context that referred to itself would have no fixed point.
    let bundle = if step.context.is_empty() {
        crate::context::Bundle::default()
    } else {
        let inner_builtins = make_builtins(cx, &step.id, visit, &prompt_file, extras);
        let inner = template::Ctx {
            vars: cx.vars,
            outputs: cx.outputs,
            step_ids: cx.step_ids,
            builtins: inner_builtins,
        };
        let containment = crate::context::Containment {
            flow_dir: cx.flow_dir,
            workspace: cx.workspace,
        };
        crate::context::build(
            cx.flow,
            &step.context,
            &containment,
            cx.flow.defaults.max_context_chars,
            &mut |t| template::render(t, &inner),
        )
        .map_err(|e| format!("step '{}': {e}", step.id))?
    };
    if !bundle.is_empty() {
        contain::write_private(&context_file, &bundle.text)
            .map_err(|e| format!("cannot write {}: {e}", context_file.display()))?;
        let manifest = serde_json::to_string_pretty(&bundle.manifest(&step.id, visit))
            .map_err(|e| format!("cannot serialize the context manifest: {e}"))?;
        contain::write_private(&cx.run_dir.join(format!("{tag}.context.json")), manifest)
            .map_err(|e| format!("cannot write the context manifest: {e}"))?;
    }
    let mut builtins = make_builtins(cx, &step.id, visit, &prompt_file, extras);
    builtins.insert("context".into(), bundle.text.clone());
    builtins.insert(
        "context_file".into(),
        if bundle.is_empty() {
            String::new()
        } else {
            context_file.display().to_string()
        },
    );
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
    // `prepend` puts the bundle in front of the prompt; `file` leaves the
    // prompt exactly as written and expects it to point at {{context_file}}.
    // A step with no context: list gets neither, byte for byte as before.
    let prompt = match (&prompt, step.context_delivery()) {
        (Some(p), flow::ContextDelivery::Prepend) if !bundle.is_empty() => {
            Some(crate::context::prepend(&bundle, p))
        }
        _ => prompt,
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
    // A step's own `cwd:` is an explicit statement and always wins. Otherwise a
    // managed workspace supplies the directory, which is the whole point of
    // having one: the run's side effects land there instead of in whatever
    // directory the caller happened to be standing in. With no workspace this
    // stays `None` and the child inherits sfh's cwd, exactly as before v1.2.
    let cwd = match &eff.cwd {
        Some(c) => Some(PathBuf::from(rend_exec("cwd", c)?)),
        None => cx.workspace.map(PathBuf::from),
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
    let mut session_parent: Option<SessionParent> = None;
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
                // Every guard above has passed, so this attachment is the one
                // the step will actually run under. Record it for step_start.
                session_parent = Some(SessionParent {
                    mode: if is_fork { "fork" } else { "continue" },
                    step: target.clone(),
                    tool: info.tool.clone(),
                    id: info.id.clone(),
                });
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
                    // The prompt is now the last argv element and must never be
                    // written down: the run dir outlives the run and the text
                    // can carry anything a template pulled in (spec P0-04).
                    let last = v.len() - 1;
                    (execute::Invocation::Argv(v).redact_argv(vec![last]), None)
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
        wall_deadline: cx.wall_deadline,
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
        session_parent,
        // Custom commands may legitimately print nothing; agent steps may not.
        allow_empty: step.allow_empty.unwrap_or(!is_preset),
        context_hash: (!bundle.is_empty()).then(|| bundle.hash.clone()),
        context_file: (!bundle.is_empty()).then_some(context_file),
        exit_conflict: step.exit_conflict(cx.flow),
        outcomes: step.outcomes.clone(),
        retry: retry_cfg(cx.flow, step),
        run_clock: cx.run_clock.cloned(),
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
    /// What the structured protocol actually proved. A preset step whose
    /// evidence does not permit success fails even when the process exited 0,
    /// and a non-zero exit is never corrected to 0 without a positively
    /// identified terminal success record (spec P0-01/P0-02).
    pub evidence: protocol::ProtocolEvidence,
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
        preset::OutputParse::Stdout => {
            let text = stdout.trim().to_string();
            let final_message_seen = !text.is_empty();
            ParsedOut {
                text,
                evidence: ProtocolEvidence {
                    final_message_seen,
                    ..ProtocolEvidence::plain()
                },
                ..Default::default()
            }
        }
        preset::OutputParse::CodexJsonl(f) => {
            let mut o = parse_codex_jsonl(stdout);
            let file_text = match run_dir {
                Some(base) => contain::read_contained_abs(base, f)
                    .map(|t| t.unwrap_or_default())
                    .map_err(|e| format!("refusing to read the codex last-message file: {e}"))?,
                None => std::fs::read_to_string(f).unwrap_or_default(),
            };
            // --output-last-message is the authoritative final answer; the
            // agent_message event is the fallback when the file is empty. Raw
            // stdout is NOT a third fallback: codex stdout is the JSONL event
            // log, and handing it on as an answer is exactly the fail-open the
            // protocol contract exists to stop.
            if !file_text.trim().is_empty() {
                o.text = file_text.trim().to_string();
                o.evidence.final_message_seen = true;
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
/// so absence of the documented `{"type":"result"}` line is the failure signal -
/// not that field. Any other trailing JSON object (a progress record, a config
/// dump) is NOT a result and must not be read as one.
fn parse_cursor_json(stdout: &str) -> ParsedOut {
    let mut o = ParsedOut::default();
    let v = match single_envelope(stdout, "cursor-agent", &|v| {
        v.get("type").and_then(|x| x.as_str()) == Some("result")
    }) {
        Ok(v) => v,
        Err(evidence) => {
            o.failed = true;
            o.evidence = evidence;
            return o;
        }
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
    // The documented subtypes are `success` and `error`; anything else is an
    // envelope shape sfh does not know how to read a verdict out of.
    match v.get("subtype").and_then(|x| x.as_str()) {
        Some("success") | None => {
            o.evidence.protocol = ProtocolState::Valid;
            o.evidence.terminal_seen = true;
            o.evidence.terminal_success = Some(true);
            o.evidence.final_message_seen = !o.text.is_empty();
        }
        Some("error") => {
            o.failed = true;
            o.evidence.protocol = ProtocolState::Valid;
            o.evidence.terminal_seen = true;
            o.evidence.terminal_success = Some(false);
        }
        Some(other) => {
            o.failed = true;
            o.text.clear();
            o.evidence = ProtocolEvidence::invalid(format!(
                "cursor-agent reported the unrecognised result subtype '{other}'; sfh will not guess whether the turn succeeded"
            ));
        }
    }
    o
}

/// pi --mode json is a potentially very large JSONL event stream. Keep its
/// semantic state separately from the bounded raw transcript: the session is
/// at the beginning, usage is spread across assistant message_end events, and
/// the only authoritative final answer is normally at the very end.
#[derive(Default)]
struct PiJsonlAccumulator {
    parsed: ParsedOut,
    input_tokens: u64,
    output_tokens: u64,
    reported_usage: preset::Usage,
    saw_usage: bool,
    malformed: u32,
}

impl PiJsonlAccumulator {
    fn push_line(&mut self, line: &[u8]) {
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            return;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            self.malformed = self.malformed.saturating_add(1);
            return;
        };
        match v.get("type").and_then(|x| x.as_str()) {
            Some("session") => {
                self.parsed.session = v.get("id").and_then(|x| x.as_str()).map(String::from);
                self.parsed.session_marker = v
                    .get("timestamp")
                    .and_then(|x| x.as_str())
                    .map(String::from);
                self.parsed.session_parent = v
                    .get("parentSession")
                    .and_then(|x| x.as_str())
                    .map(String::from);
            }
            Some("message_end") => {
                let Some(m) = v.get("message") else { return };
                if m.get("role").and_then(|x| x.as_str()) != Some("assistant") {
                    return;
                }
                // Later assistant messages replace earlier ones (tool-use
                // turns and provider retries). Only text blocks are chain data.
                self.parsed.text = m
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
                // An assistant message_end is pi's terminal record for a turn.
                // Later ones replace earlier ones, so the verdict is the last
                // one's, not a sticky OR of every turn.
                self.parsed.evidence.terminal_seen = true;
                self.parsed.evidence.final_message_seen = !self.parsed.text.is_empty();
                if matches!(
                    m.get("stopReason").and_then(|x| x.as_str()),
                    Some("error") | Some("aborted")
                ) {
                    self.parsed.failed = true;
                    self.parsed.evidence.terminal_success = Some(false);
                } else {
                    self.parsed.evidence.terminal_success = Some(true);
                }
                if let Some(u) = m.get("usage") {
                    self.saw_usage = true;
                    self.input_tokens = self
                        .input_tokens
                        .saturating_add(u.get("input").and_then(|x| x.as_u64()).unwrap_or(0));
                    self.output_tokens = self
                        .output_tokens
                        .saturating_add(u.get("output").and_then(|x| x.as_u64()).unwrap_or(0));
                    if let Some(cost) = u
                        .get("cost")
                        .and_then(|c| c.get("total"))
                        .and_then(|x| x.as_f64())
                    {
                        self.reported_usage.add_reported_cost(cost);
                    }
                }
            }
            _ => {}
        }
    }

    fn finish(mut self) -> ParsedOut {
        if self.saw_usage {
            self.parsed.usage.input_tokens = Some(self.input_tokens);
            self.parsed.usage.output_tokens = Some(self.output_tokens);
            if self.reported_usage.cost_usd.is_none() {
                self.reported_usage.cost_usd = Some(0.0);
            }
            self.parsed.usage.cost_usd = self.reported_usage.cost_usd;
            self.parsed.usage.invalid_cost = self.reported_usage.invalid_cost;
        }
        let malformed = self.malformed;
        finish_stream_evidence(&mut self.parsed, "pi", malformed, "JSONL");
        self.parsed
    }
}

/// A single JSONL record is bounded independently of the full transcript. Real
/// pi records are normally KiB; allowing 16 MiB covers large tool results while
/// preventing an unterminated/malicious line from turning streaming parsing
/// back into unbounded memory growth.
const MAX_PI_JSONL_LINE: usize = 16 * 1024 * 1024;

#[derive(Default)]
struct PiStreamState {
    accumulator: PiJsonlAccumulator,
    pending: Vec<u8>,
    discarding_oversized_line: bool,
    oversized_line: bool,
}

#[derive(Default)]
struct PiStreamObserver {
    state: Mutex<PiStreamState>,
}

impl OutputObserver for PiStreamObserver {
    fn observe(&self, chunk: &[u8]) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        for segment in chunk.split_inclusive(|b| *b == b'\n') {
            let ends_line = segment.last() == Some(&b'\n');
            if state.discarding_oversized_line {
                if ends_line {
                    state.discarding_oversized_line = false;
                }
                continue;
            }
            if state.pending.len().saturating_add(segment.len()) > MAX_PI_JSONL_LINE {
                state.pending.clear();
                state.oversized_line = true;
                state.discarding_oversized_line = !ends_line;
                continue;
            }
            state.pending.extend_from_slice(segment);
            if ends_line {
                let line = std::mem::take(&mut state.pending);
                state.accumulator.push_line(trim_ascii_line(&line));
            }
        }
    }
}

impl PiStreamObserver {
    fn finish(&self) -> (ParsedOut, Option<String>) {
        let Ok(mut state) = self.state.lock() else {
            let why = "pi JSONL semantic observer lock was poisoned";
            return (
                ParsedOut {
                    failed: true,
                    evidence: ProtocolEvidence::invalid(why),
                    ..Default::default()
                },
                Some(why.into()),
            );
        };
        if !state.pending.is_empty() && !state.discarding_oversized_line {
            let line = std::mem::take(&mut state.pending);
            state.accumulator.push_line(trim_ascii_line(&line));
        }
        let oversized = state.oversized_line || state.discarding_oversized_line;
        let mut parsed = std::mem::take(&mut state.accumulator).finish();
        let diagnostic = if oversized {
            let why = format!(
                "pi JSONL contained a record larger than {} MiB; final output and accounting cannot be verified",
                MAX_PI_JSONL_LINE / 1024 / 1024
            );
            parsed.failed = true;
            parsed.evidence = ProtocolEvidence::invalid(why.clone());
            Some(why)
        } else {
            parsed.evidence.diagnostic.clone()
        };
        (parsed, diagnostic)
    }
}

fn trim_ascii_line(mut line: &[u8]) -> &[u8] {
    while line.last().is_some_and(|b| matches!(b, b'\r' | b'\n')) {
        line = &line[..line.len() - 1];
    }
    line
}

/// Non-streaming parser used by doctor/tests and as a compatibility fallback.
fn parse_pi_jsonl(stdout: &str) -> ParsedOut {
    let mut accumulator = PiJsonlAccumulator::default();
    for line in stdout.lines() {
        accumulator.push_line(line.as_bytes());
    }
    accumulator.finish()
}

/// Execute one prepared leaf, honouring its retry policy.
pub fn exec_leaf(prep: Prepared) -> LeafDone {
    let cfg = prep.retry;
    let mut attempt = 0u32;
    let mut cumulative_usage = preset::Usage::default();
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
        cumulative_usage.accumulate(&done.usage);
        done.usage = cumulative_usage.clone();
        done.attempts = attempt + 1;
        if done.ok() || done.interrupted || done.persistence_error.is_some() || attempt >= cfg.max {
            return done;
        }
        let retryable = match cfg.mode {
            RetryMode::Never => false,
            RetryMode::Any => true,
            // A timeout used to be categorically non-transient, which was right
            // for "the model was still working when the clock ran out" and
            // wrong for "the pipe went dead 38 minutes ago" (B-12). The idle
            // clock separates them: silence longer than hang_after_sec is a
            // hang, and a hang is exactly the kind of failure a retry fixes.
            // A declared outcome replaces the guess entirely. Needle-matching
            // exists because sfh usually cannot know why a step failed; when
            // the flow has said, guessing on top of that is not caution, it is
            // sfh overruling a statement it asked for.
            RetryMode::Transient if done.outcome.is_some() => {
                matches!(
                    done.outcome.as_ref().map(|(r, _)| *r),
                    Some(flow::OutcomeResult::Retryable)
                )
            }
            RetryMode::Transient => {
                // A PRESET step's chain output is the tool's own report, so a
                // rate limit or a serving abort in it really does describe how
                // this attempt failed. A custom `cmd:` step's stdout is its
                // RESULT - the test list, the diff, the JSON it was asked to
                // produce - and scanning that for provider needles reads the
                // command's data as if it were a statement about the harness.
                //
                // A verification step whose suite contains a case named
                // `tcp_502_returns_error` was therefore re-run in full on every
                // deterministic failure: minutes of compute and a second bill,
                // for a match on the word 502 inside a test name. stderr is
                // still read for both, because that is where a command reports
                // operational trouble - `curl` writing "connection reset" is
                // exactly the transient case, and it goes there.
                let tool_report = if done.tool.is_some() {
                    done.chain_output.as_str()
                } else {
                    ""
                };
                (!done.timed_out && execute::is_transient_failure(&done.stderr_clean, tool_report))
                    || (done.timed_out && done.idle_ms >= cfg.hang_after_sec.saturating_mul(1000))
            }
        };
        if !retryable {
            return done;
        }
        let wait = cfg.backoff_sec.saturating_mul(1u64 << attempt.min(5));
        if !prep.quiet {
            let why = if done.timed_out {
                format!("timed out after {}s of silence", done.idle_ms / 1000)
            } else {
                format!("exit={}", done.exit_code)
            };
            eprintln!(
                "sfh: [{}] transient failure ({why}), retrying in {wait}s ({}/{})",
                prep.tag,
                attempt + 1,
                cfg.max
            );
        }
        let retry_deadline = std::time::Instant::now() + Duration::from_secs(wait);
        let deadline = prep
            .wall_deadline
            .map(|wall| wall.min(retry_deadline))
            .unwrap_or(retry_deadline);
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
            let _ = contain::write_private_atomic(&p.err_file, &why);
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
            idle_ms: 0,
            attempts: 1,
            chain_output: String::new(),
            stderr_clean: why.clone(),
            out_file: p.out_file,
            session_id: None,
            session_marker: None,
            tool: p.tool,
            cwd: cwd_str,
            access: p.access,
            usage: preset::Usage::default(),
            cmd: cmd_desc,
            harness_diagnostic: Some(why.clone()),
            // No `step_end` may certify completion when an artifact path is
            // unsafe. The engine treats this like any other durability error
            // and aborts the run without recording a reusable completion.
            persistence_error: Some(why.clone()),
            // Nothing ran, so there is no exit code for an `outcomes:` table
            // to describe.
            outcome: None,
            protocol: ProtocolEvidence::default(),
        };
    }
    // Compute the remaining wall budget at the last possible point before
    // spawning. Artifact validation and no-follow pre-creation can perform
    // filesystem I/O; measuring before them let a scheduling/filesystem delay
    // leak past the absolute run deadline.
    let mut deadline_expired = false;
    let timeout = match p.wall_deadline {
        Some(deadline) => {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                deadline_expired = true;
                None
            } else {
                Some(p.timeout.map_or(remaining, |step| step.min(remaining)))
            }
        }
        None => p.timeout,
    };
    if deadline_expired {
        let why = "run wall_clock_sec expired before this queued leaf could start";
        let persistence_error =
            persist_prestart_failure(&p.out_file, &p.err_file, &p.chain_file, why);
        return LeafDone {
            tag: p.tag,
            exit_code: -1,
            timed_out: true,
            interrupted: false,
            dur_ms: 0,
            idle_ms: 0,
            attempts: 1,
            chain_output: String::new(),
            stderr_clean: why.into(),
            out_file: p.out_file,
            session_id: None,
            session_marker: None,
            tool: p.tool,
            cwd: cwd_str,
            access: p.access,
            usage: preset::Usage::default(),
            cmd: cmd_desc,
            harness_diagnostic: Some(why.into()),
            persistence_error,
            outcome: None,
            protocol: ProtocolEvidence::default(),
        };
    }
    // Pi emits an unbounded JSONL event transcript. Parse its semantic records
    // on the pipe reader thread so the raw artifact's bounded capture cannot
    // discard the terminal answer or later usage/cost reports.
    let pi_observer = matches!(p.parse, preset::OutputParse::PiJsonl)
        .then(|| Arc::new(PiStreamObserver::default()));
    let stdout_observer = pi_observer
        .as_ref()
        .map(|observer| Arc::clone(observer) as Arc<dyn execute::OutputObserver>);
    let outcome = match execute::run_cmd(
        &p.inv,
        p.stdin_payload,
        p.cwd.as_deref(),
        timeout,
        &p.env_remove,
        &p.env_set,
        execute::Observe {
            // Tee to the step's out file so a long step is observable while it
            // runs; the cleaned text replaces it once the child exits.
            tee: Some(p.out_file.clone()),
            stdout_observer,
            run_clock: p.run_clock.clone(),
        },
    ) {
        Ok(o) => o,
        Err(e) => {
            // A failed spawn is still a completed, routable leaf result. Give
            // resume the same three-artifact set as a started process; if any
            // write fails, do not record a reusable step_end.
            let persistence_error =
                persist_prestart_failure(&p.out_file, &p.err_file, &p.chain_file, &e);
            if !p.quiet {
                eprintln!("sfh: [{}] spawn failed: {e}", p.tag);
            }
            return LeafDone {
                tag: p.tag,
                exit_code: -1,
                timed_out: false,
                interrupted: execute::interrupted(),
                dur_ms: 0,
                idle_ms: 0,
                attempts: 1,
                chain_output: String::new(),
                stderr_clean: e.clone(),
                out_file: p.out_file,
                session_id: None,
                session_marker: None,
                tool: p.tool,
                cwd: cwd_str,
                access: p.access,
                usage: preset::Usage::default(),
                cmd: cmd_desc,
                harness_diagnostic: Some(format!("failed to spawn tool: {e}")),
                persistence_error,
                outcome: None,
                protocol: ProtocolEvidence::default(),
            };
        }
    };
    let stdout_clean = clean_text(&outcome.stdout);
    let mut stderr_clean = clean_text(&outcome.stderr);
    let pi_stream = pi_observer.as_ref().map(|observer| observer.finish());
    let mut harness_diagnostic = pi_stream
        .as_ref()
        .and_then(|(_, diagnostic)| diagnostic.clone());
    if let Some(diagnostic) = &harness_diagnostic {
        stderr_clean.push_str(&format!("\nsfh: {diagnostic}\n"));
    }
    let mut persistence_error = None;
    if let Err(e) = contain::write_private_atomic(&p.out_file, &stdout_clean) {
        persistence_error = Some(format!(
            "cannot persist required output artifact {}: {e}",
            p.out_file.display()
        ));
    }
    if let Err(e) = contain::write_private_atomic(&p.err_file, &stderr_clean) {
        persistence_error.get_or_insert_with(|| {
            format!(
                "cannot persist required stderr artifact {}: {e}",
                p.err_file.display()
            )
        });
    }

    let mut parsed = if let Some((parsed, _)) = pi_stream {
        parsed
    } else {
        match parse_output(&p.parse, &stdout_clean, &stderr_clean, Some(&p.run_dir)) {
            Ok(o) => o,
            Err(e) => {
                // A containment violation reading the tool's artifact is a failure
                // of this step, not empty output (rev_break #4).
                stderr_clean.push_str(&format!("\nsfh: {e}\n"));
                harness_diagnostic = Some(e.clone());
                if let Err(write_err) = contain::write_private_atomic(&p.err_file, &stderr_clean) {
                    persistence_error.get_or_insert_with(|| {
                        format!(
                            "cannot persist required stderr artifact {}: {write_err}",
                            p.err_file.display()
                        )
                    });
                }
                ParsedOut {
                    failed: true,
                    ..Default::default()
                }
            }
        }
    };
    if parsed.usage.sanitize_reported() {
        let warning = "provider reported an invalid cost_usd; the value was normalized without refunding prior spend";
        eprintln!("sfh: warning: [{}] {warning}", p.tag);
        stderr_clean.push_str(&format!("\nsfh: {warning}\n"));
        if let Err(e) = contain::write_private_atomic(&p.err_file, &stderr_clean) {
            persistence_error.get_or_insert_with(|| {
                format!(
                    "cannot persist required stderr artifact {}: {e}",
                    p.err_file.display()
                )
            });
        }
    }
    let mut exit_code = outcome.exit_code;
    // Several tools report success/failure in-band and get the exit code wrong,
    // in BOTH directions. The two corrections are not symmetric:
    //
    //  - a documented in-band failure always beats a zero exit, and
    //  - a non-zero exit is only ever excused by a positively identified
    //    terminal SUCCESS record (spec P0-01).
    //
    // Before v1.2 the second correction fired for agy on "non-empty text and
    // nothing said it failed", and agy's parser handed raw stdout back as that
    // text whenever it could not parse an envelope. A usage error printed to
    // stdout with exit 1 therefore became a successful step whose answer was
    // the usage message. `certifies_success` cannot be satisfied by raw text,
    // an unknown status, a malformed envelope or a missing terminal record.
    //
    // Which way the second correction falls is the flow's call, not a hardcoded
    // adapter list: v1.2.0 excused only agy, and a CLI that exits 1 because
    // some intermediate tool call failed - after producing and committing a
    // complete final answer - had no way to say so except to stop checking exit
    // codes at all, which is fail-open. `exit_conflict: trust_protocol` is the
    // narrow, declared alternative, and it still cannot fire without positive
    // evidence of success. The default stays `fail` for every tool whose exit
    // status is trustworthy, so no existing flow changes meaning.
    // An incomplete structured protocol is recorded FIRST, whichever way the
    // exit code lands. "The tool failed" and "sfh could not verify that the
    // tool finished" are different diagnoses, and only one of them tells a user
    // that the CLI's output shape has drifted out from under their flow.
    // Custom `cmd:` steps are ProtocolState::Plain and keep their long-standing
    // stdout contract untouched.
    let protocol_failure = (!outcome.timed_out && !parsed.evidence.allows_success())
        .then(|| {
            parsed
                .evidence
                .failure_reason(p.tool.as_deref().unwrap_or("the tool"))
        })
        .flatten();
    if let Some(why) = &protocol_failure {
        stderr_clean.push_str(&format!("\nsfh: {why}\n"));
        harness_diagnostic.get_or_insert_with(|| why.clone());
        if let Err(e) = contain::write_private_atomic(&p.err_file, &stderr_clean) {
            persistence_error.get_or_insert_with(|| {
                format!(
                    "cannot persist required stderr artifact {}: {e}",
                    p.err_file.display()
                )
            });
        }
    }
    // What this step's own exit code MEANS, if the flow said. Read from the RAW
    // process exit, before any correction: `outcomes: {2: ...}` is a statement
    // about what the command prints on its way out, and a table that shifted
    // under sfh's own adjustments would mean something different from what it
    // says.
    //
    // Deliberately gated on the protocol first. A declared outcome describes a
    // command that RAN and reported; it is not a licence to accept a turn whose
    // structured protocol never completed. Fail-closed keeps precedence, so a
    // preset step whose stream died mid-answer cannot be relabelled "complete"
    // by an exit-code table.
    let declared = (protocol_failure.is_none() && !outcome.timed_out && !outcome.interrupted)
        .then(|| p.outcomes.get(&outcome.exit_code))
        .flatten()
        .cloned();
    if let Some(d) = &declared {
        if d.result.is_success() {
            // "Ran fine, there is more to do" is not a failure, and must not be
            // read as one: no on_error, no retry, no partial-emit path.
            exit_code = 0;
        } else if exit_code == 0 {
            // A flow that calls exit 0 a failure is unusual but coherent, and
            // saying so must actually fail the step.
            exit_code = 1;
        }
    }
    // "The flow said nothing" and "the flow said fail" are different: only the
    // second may override an adapter documented to get its exit codes wrong.
    let trust_protocol = match p.exit_conflict {
        Some(flow::ExitConflict::TrustProtocol) => true,
        Some(flow::ExitConflict::Fail) => false,
        None => !preset::exit_code_trustworthy(p.tool.as_deref().unwrap_or("")),
    };
    // Neither correction may run over a step that was killed or that said, in
    // its own protocol, that it failed. A terminal success record can arrive
    // and the process still be cut down afterwards by `sfh stop` or Ctrl-C -
    // reporting that as a completed step would turn "a human stopped this" into
    // "this finished", which is the one thing a stopped run must never say.
    if declared.is_some() {
        // The flow has spoken about this exit code. Neither the in-band failure
        // fold nor the exit_conflict correction gets a second opinion.
    } else if (parsed.failed || protocol_failure.is_some()) && exit_code == 0 {
        exit_code = 1;
    } else if exit_code != 0
        && !outcome.timed_out
        && !outcome.interrupted
        && !parsed.failed
        && parsed.evidence.certifies_success()
    {
        if trust_protocol {
            exit_code = 0;
        } else {
            // The step still fails - but silently reporting only the exit code
            // hides that sfh held proof of a completed turn, which is the one
            // fact that tells a user whether to reach for `exit_conflict:` or
            // to go looking for a real failure.
            let why = format!(
                "{} exited {} but its own protocol certified this turn as successful \
                 (terminal record found, status success). sfh failed the step because an \
                 exit code is a failure unless the flow says otherwise. If this tool is \
                 known to exit non-zero without invalidating its answer, declare \
                 `exit_conflict: trust_protocol` on the step or in defaults - do not stop \
                 checking exit codes.",
                p.tool.as_deref().unwrap_or("the tool"),
                outcome.exit_code
            );
            stderr_clean.push_str(&format!("\nsfh: {why}\n"));
            harness_diagnostic.get_or_insert(why);
            if let Err(e) = contain::write_private_atomic(&p.err_file, &stderr_clean) {
                persistence_error.get_or_insert_with(|| {
                    format!(
                        "cannot persist required stderr artifact {}: {e}",
                        p.err_file.display()
                    )
                });
            }
        }
    }
    let protocol_evidence = parsed.evidence.clone();
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
            harness_diagnostic = Some(why.trim().trim_start_matches("sfh: ").to_string());
            if !why.starts_with("\nsfh: the tool exited successfully") {
                session_id = None;
            }
            stderr_clean.push_str(&why);
            if let Err(e) = contain::write_private_atomic(&p.err_file, &stderr_clean) {
                persistence_error.get_or_insert_with(|| {
                    format!(
                        "cannot persist required stderr artifact {}: {e}",
                        p.err_file.display()
                    )
                });
            }
        }
    }
    if let Err(e) = contain::write_private_atomic(&p.chain_file, &chain_output) {
        persistence_error.get_or_insert_with(|| {
            format!(
                "cannot persist required chain artifact {}: {e}",
                p.chain_file.display()
            )
        });
    }
    if let Some(e) = &persistence_error {
        exit_code = -1;
        harness_diagnostic = Some(e.clone());
        stderr_clean.push_str(&format!("\nsfh: {e}\n"));
    }

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
        // The only site where a process really ran, so the only one that can
        // carry what its exit code was declared to mean.
        outcome: declared.map(|d| (d.result, d.label)),
        exit_code,
        timed_out: outcome.timed_out,
        interrupted: outcome.interrupted,
        dur_ms: outcome.dur_ms,
        idle_ms: outcome.idle_ms,
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
        harness_diagnostic,
        persistence_error,
        protocol: protocol_evidence,
    }
}

fn persist_prestart_failure(
    out_file: &Path,
    err_file: &Path,
    chain_file: &Path,
    why: &str,
) -> Option<String> {
    let mut error = None;
    for (path, text, kind) in [
        (out_file, "", "output"),
        (err_file, why, "stderr"),
        (chain_file, "", "chain"),
    ] {
        if let Err(e) = contain::write_private_atomic(path, text) {
            error.get_or_insert_with(|| {
                format!(
                    "cannot persist required {kind} artifact {}: {e}",
                    path.display()
                )
            });
        }
    }
    error
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
///
/// `turn.completed` / `turn.failed` is codex's documented terminal record. A
/// stream that stops before one of them describes a turn nobody can say
/// finished, so it is `missing_terminal` rather than "whatever text we saw".
fn parse_codex_jsonl(stdout: &str) -> ParsedOut {
    let mut o = ParsedOut::default();
    let mut malformed = 0u32;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            malformed = malformed.saturating_add(1);
            continue;
        };
        match v.get("type").and_then(|x| x.as_str()) {
            Some("thread.started") => {
                if let Some(id) = v.get("thread_id").and_then(|x| x.as_str()) {
                    o.session = Some(id.to_string());
                }
            }
            Some("turn.completed") => {
                o.evidence.terminal_seen = true;
                o.evidence.terminal_success = Some(true);
                if let Some(u) = v.get("usage") {
                    o.usage.input_tokens = num(u.get("input_tokens"));
                    o.usage.output_tokens = num(u.get("output_tokens"));
                }
            }
            Some("turn.failed") => {
                o.failed = true;
                o.evidence.terminal_seen = true;
                o.evidence.terminal_success = Some(false);
            }
            Some("item.completed") => {
                if let Some(item) = v.get("item") {
                    if item.get("type").and_then(|x| x.as_str()) == Some("agent_message") {
                        if let Some(t) = item.get("text").and_then(|x| x.as_str()) {
                            o.text = t.trim().to_string();
                            o.evidence.final_message_seen = !o.text.is_empty();
                        }
                    }
                }
            }
            _ => {}
        }
    }
    finish_stream_evidence(&mut o, "codex", malformed, "JSONL");
    o
}

/// Shared close-out for the line-oriented adapters: a stream that carried a
/// record sfh could not parse is not the documented format, and one that never
/// reached its terminal record cannot be certified as a finished turn.
fn finish_stream_evidence(o: &mut ParsedOut, tool: &str, malformed: u32, shape: &str) {
    o.evidence.malformed_records = malformed;
    if malformed > 0 {
        o.evidence.protocol = ProtocolState::Invalid;
        o.evidence.diagnostic = Some(format!(
            "{tool} {shape} stdout contained {malformed} record(s) sfh could not parse; the machine-readable protocol did not hold, so its output is not a usable answer"
        ));
    } else if o.evidence.terminal_seen {
        o.evidence.protocol = ProtocolState::Valid;
    } else {
        o.evidence.protocol = ProtocolState::MissingTerminal;
        o.evidence.diagnostic = Some(format!(
            "{tool} {shape} stdout ended without its documented terminal record, so sfh cannot tell whether the turn finished"
        ));
    }
}

/// A record size beyond which a "single envelope" adapter is no longer being
/// parsed incrementally in any meaningful sense. The stream adapters have their
/// own per-record cap; this is the equivalent hard stop for the ones that are
/// documented to print exactly one JSON document (spec 5.3).
const MAX_ENVELOPE_BYTES: usize = 16 * 1024 * 1024;

/// Locate the documented terminal envelope of a single-document adapter.
///
/// The tool is documented to print one JSON value. Try that first, so a
/// pretty-printed object (grok) is read as the single document it is; only when
/// that fails is the output treated as lines, and then every non-blank line
/// that is not JSON is a record the adapter never promised - the stream is not
/// the documented format and no text from it may be handed on as an answer.
fn single_envelope(
    stdout: &str,
    tool: &str,
    is_terminal: &dyn Fn(&serde_json::Value) -> bool,
) -> Result<serde_json::Value, ProtocolEvidence> {
    let t = stdout.trim();
    if t.len() > MAX_ENVELOPE_BYTES {
        return Err(ProtocolEvidence::invalid(format!(
            "{tool} produced more than {} MiB of stdout for a single-envelope protocol; sfh will not guess the final answer or the accounting from it",
            MAX_ENVELOPE_BYTES / 1024 / 1024
        )));
    }
    if t.is_empty() {
        return Err(ProtocolEvidence {
            protocol: ProtocolState::MissingTerminal,
            diagnostic: Some(format!(
                "{tool} produced no output at all, so its documented result envelope is missing"
            )),
            ..Default::default()
        });
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
        if is_terminal(&v) {
            return Ok(v);
        }
        return Err(ProtocolEvidence {
            protocol: ProtocolState::MissingTerminal,
            diagnostic: Some(format!(
                "{tool} printed JSON that is not its documented result envelope, so sfh cannot tell whether the turn finished"
            )),
            ..Default::default()
        });
    }
    let mut malformed = 0u32;
    let mut terminal = None;
    for line in t.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) => {
                if is_terminal(&v) {
                    terminal = Some(v);
                }
            }
            Err(_) => malformed = malformed.saturating_add(1),
        }
    }
    // Several of these CLIs print a human banner before the envelope
    // (cursor-agent's "Using worktree: ..."), which is a form they officially
    // emit, so it is not by itself a broken protocol - the envelope still has
    // to be there. When it is NOT there, the leftover text is not promoted to
    // an answer: that is the fail-open this contract removes.
    match terminal {
        Some(v) => Ok(v),
        None if malformed > 0 => Err(ProtocolEvidence {
            protocol: ProtocolState::Invalid,
            malformed_records: malformed,
            diagnostic: Some(format!(
                "{tool} stdout held {malformed} record(s) that are not its documented machine-readable output and no result envelope; sfh will not treat that text as the answer"
            )),
            ..Default::default()
        }),
        None => Err(ProtocolEvidence {
            protocol: ProtocolState::MissingTerminal,
            diagnostic: Some(format!(
                "{tool} never printed its documented result envelope, so sfh cannot tell whether the turn finished"
            )),
            ..Default::default()
        }),
    }
}

/// claude --output-format json: one envelope with .result/.session_id/.total_cost_usd.
/// The documented terminal record is `{"type":"result", ...}`; `is_error`
/// carries its verdict.
fn parse_claude_json(stdout: &str) -> ParsedOut {
    let mut o = ParsedOut::default();
    let v = match single_envelope(stdout, "claude", &|v| {
        v.get("type").and_then(|x| x.as_str()) == Some("result")
    }) {
        Ok(v) => v,
        Err(evidence) => {
            o.failed = true;
            o.evidence = evidence;
            return o;
        }
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
    o.evidence.protocol = ProtocolState::Valid;
    o.evidence.terminal_seen = true;
    o.evidence.terminal_success = Some(!o.failed);
    o.evidence.final_message_seen = !o.text.is_empty();
    o
}

/// opencode --format json: NDJSON events; final answer = concat of `text` events
/// belonging to the last message (dedupe by part id, keep last occurrence).
fn parse_opencode_ndjson(stdout: &str) -> ParsedOut {
    let mut o = ParsedOut::default();
    let mut malformed = 0u32;
    let mut texts: Vec<(String, String, String)> = Vec::new(); // (part_id, message_id, text)
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v = match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) => v,
            Err(_) => {
                malformed = malformed.saturating_add(1);
                continue;
            }
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
            Some("step_finish") | Some("finish") => {
                o.evidence.terminal_seen = true;
                o.evidence.terminal_success.get_or_insert(true);
                if let Some(part) = v.get("part") {
                    if let Some(tk) = part.get("tokens") {
                        o.usage.input_tokens = num(tk.get("input"));
                        o.usage.output_tokens = num(tk.get("output"));
                    }
                    if let Some(c) = part.get("cost").and_then(|x| x.as_f64()) {
                        o.usage.add_reported_cost(c);
                    }
                }
            }
            Some("error") => {
                o.failed = true;
                o.evidence.terminal_seen = true;
                o.evidence.terminal_success = Some(false);
            }
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
    o.evidence.final_message_seen = !o.text.is_empty();
    finish_stream_evidence(&mut o, "opencode", malformed, "NDJSON");
    o
}

/// grok --output-format json: one pretty-printed object with .text/.sessionId,
/// or an `{"type":"error", ...}` object. Either is a terminal record; anything
/// else (including a truncated pretty-printed object) is not.
fn parse_grok_json(stdout: &str) -> ParsedOut {
    let mut o = ParsedOut::default();
    let v = match single_envelope(stdout, "grok", &|v| {
        v.get("type").and_then(|x| x.as_str()) == Some("error") || v.get("text").is_some()
    }) {
        Ok(v) => v,
        Err(evidence) => {
            o.failed = true;
            o.evidence = evidence;
            return o;
        }
    };
    o.evidence.protocol = ProtocolState::Valid;
    o.evidence.terminal_seen = true;
    if v.get("type").and_then(|x| x.as_str()) == Some("error") {
        o.failed = true;
        o.evidence.terminal_success = Some(false);
        o.text = v
            .get("message")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        return o;
    }
    o.evidence.terminal_success = Some(true);
    o.text = v
        .get("text")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    o.evidence.final_message_seen = !o.text.is_empty();
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

/// The documented result record of an agy envelope, whether it arrived bare or
/// wrapped by stream-json as `{"event":"result","result":{...}}`.
fn agy_result_record(v: &serde_json::Value) -> Option<&serde_json::Value> {
    let obj = match v.get("event").and_then(|x| x.as_str()) {
        Some("result") => v.get("result")?,
        Some(_) => return None,
        // Bare `--output-format json`: the object itself is the result record.
        None => v,
    };
    // `status` is what makes this the terminal record rather than a progress
    // object that merely happens to carry a `response` field.
    obj.get("status").and_then(|x| x.as_str())?;
    Some(obj)
}

/// agy --output-format json: {response, status, conversation_id, usage};
/// stream-json wraps it as {"event":"result","result":{...}}.
///
/// This parser is the P0-01 blocker. Its old raw-stdout fallback combined with
/// the execution layer's agy-only exit-code correction to turn `agy: unknown
/// flag ...` + exit 1 into a successful step whose "answer" was the usage
/// message. Nothing but a recognised terminal status may certify this run now.
fn parse_agy_json(stdout: &str) -> ParsedOut {
    let mut o = ParsedOut::default();
    let v = match single_envelope(stdout, "agy", &|v| agy_result_record(v).is_some()) {
        Ok(v) => v,
        Err(evidence) => {
            o.failed = true;
            o.evidence = evidence;
            return o;
        }
    };
    let obj = match agy_result_record(&v) {
        Some(obj) => obj.clone(),
        None => {
            o.failed = true;
            o.evidence = ProtocolEvidence {
                protocol: ProtocolState::MissingTerminal,
                diagnostic: Some(
                    "agy printed no result record carrying a status field, so sfh cannot tell whether the turn finished".into(),
                ),
                ..Default::default()
            };
            return o;
        }
    };
    let status = obj.get("status").and_then(|x| x.as_str()).unwrap_or("");
    // An unrecognised status is not a success and not a documented failure - it
    // is an envelope sfh does not understand, and guessing either way is how a
    // broken invocation becomes a green step.
    let terminal_success = match status.to_ascii_uppercase().as_str() {
        "SUCCESS" | "OK" | "COMPLETED" | "DONE" => Some(true),
        "ERROR" | "FAILED" | "FAILURE" | "CANCELLED" | "CANCELED" | "TIMEOUT" => Some(false),
        _ => None,
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
    if let Some(u) = obj.get("usage") {
        o.usage.input_tokens = num(u.get("input_tokens"));
        o.usage.output_tokens = num(u.get("output_tokens"));
    }
    match terminal_success {
        Some(ok) => {
            o.failed = !ok;
            o.evidence.protocol = ProtocolState::Valid;
            o.evidence.terminal_seen = true;
            o.evidence.terminal_success = Some(ok);
            o.evidence.final_message_seen = !o.text.is_empty();
        }
        None => {
            o.failed = true;
            // The envelope parsed, but sfh cannot read a verdict out of it, so
            // its `response` is not an answer any downstream step may consume.
            o.text.clear();
            o.evidence = ProtocolEvidence::invalid(format!(
                "agy reported the unrecognised terminal status '{status}'; sfh will not guess whether the turn succeeded"
            ));
        }
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
    pub fn new(mut limits: HashMap<String, u32>) -> Arc<ToolGate> {
        // Flow validation rejects zero. Keep this constructor safe as well:
        // tests and future callers can build a gate directly, and a zero here
        // used to wait on the condition variable forever before any child was
        // spawned (so neither the step timeout nor idle detection could help).
        for limit in limits.values_mut() {
            *limit = (*limit).max(1);
        }
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

/// Run prepared leaves on a bounded worker pool. `on_done` runs in the caller
/// thread as soon as each member completes, before the remaining workers are
/// joined. The engine uses that boundary to durably record the member so a
/// crash while a slower sibling is still running cannot bill it twice.
///
/// The result Vec ALWAYS has the same length and order as the input: a slot
/// whose worker died is filled with a synthetic failure instead of being
/// dropped (positional consumers zip these against child/item lists).
pub fn run_pool<S, F>(
    preps: Vec<Prepared>,
    max_parallel: usize,
    gate: Arc<ToolGate>,
    on_start: S,
    mut on_done: F,
) -> Result<Vec<LeafDone>, String>
where
    S: Fn(usize) + Send + Sync + 'static,
    F: FnMut(usize, &LeafDone) -> Result<(), String>,
{
    let n = preps.len();
    if n == 0 {
        return Ok(Vec::new());
    }
    let warmup = Arc::new(Warmup::from(
        &preps,
        preps.first().map(|p| p.quiet).unwrap_or(true),
    ));
    if n == 1 || max_parallel <= 1 {
        let mut out = Vec::with_capacity(n);
        for (idx, p) in preps.into_iter().enumerate() {
            let held = gate.acquire(&p.tool);
            on_start(idx);
            let d = exec_leaf(p);
            gate.release(held);
            on_done(idx, &d)?;
            out.push(d);
        }
        return Ok(out);
    }
    let queue: Arc<Mutex<VecDeque<(usize, Prepared)>>> =
        Arc::new(Mutex::new(preps.into_iter().enumerate().collect()));
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let starter = Arc::new(on_start);
    let (tx, rx) = mpsc::channel::<(usize, LeafDone)>();
    let workers = max_parallel.min(n);
    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let q = Arc::clone(&queue);
        let g = Arc::clone(&gate);
        let w = Arc::clone(&warmup);
        let c = Arc::clone(&cancelled);
        let start = Arc::clone(&starter);
        let sender = tx.clone();
        handles.push(std::thread::spawn(move || loop {
            if c.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let job = match q.lock() {
                Ok(mut guard) => guard.pop_front(),
                Err(mut poisoned) => poisoned.get_mut().pop_front(),
            };
            let Some((idx, p)) = job else { break };
            let role = w.enter(&p);
            let held = g.acquire(&p.tool);
            let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                start(idx);
                exec_leaf(p)
            }))
            .unwrap_or_else(|_| synthetic_failure(idx));
            g.release(held);
            w.leave(role);
            if sender.send((idx, done)).is_err() {
                break;
            }
        }));
    }
    drop(tx);
    let mut slots: Vec<Option<LeafDone>> = (0..n).map(|_| None).collect();
    let mut callback_error = None;
    for (idx, done) in rx {
        if callback_error.is_none() {
            if let Err(e) = on_done(idx, &done) {
                cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                if let Ok(mut q) = queue.lock() {
                    q.clear();
                }
                // A completion that cannot be durably recorded is a run-level
                // integrity failure. Stop paid siblings that are already in
                // flight as well as clearing the queue; otherwise they could
                // keep spending while the run has already become unsafe to
                // resume.
                execute::request_interrupt();
                callback_error = Some(e);
            }
        }
        slots[idx] = Some(done);
    }
    for h in handles {
        let _ = h.join();
    }
    if let Some(e) = callback_error {
        return Err(e);
    }
    Ok(slots
        .into_iter()
        .enumerate()
        .map(|(i, o)| o.unwrap_or_else(|| synthetic_failure(i)))
        .collect())
}

fn synthetic_failure(idx: usize) -> LeafDone {
    LeafDone {
        tag: format!("slot-{idx}"),
        exit_code: -1,
        timed_out: false,
        interrupted: false,
        dur_ms: 0,
        idle_ms: 0,
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
        harness_diagnostic: Some("worker thread died before producing a result".into()),
        persistence_error: None,
        outcome: None,
        protocol: ProtocolEvidence::invalid("worker thread died before producing a result"),
    }
}

/// Format-valid UUIDv4 from OS-seeded hasher entropy (no external crates).
pub fn gen_uuid() -> String {
    use std::fmt::Write as _;
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
        let mut out = String::with_capacity(r.len() * 2);
        for byte in &bytes[r] {
            write!(&mut out, "{byte:02x}").expect("writing to a String cannot fail");
        }
        out
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
    fn zero_tool_gate_limit_is_defensively_bounded_instead_of_deadlocking() {
        let gate = ToolGate::new(HashMap::from([("claude".to_string(), 0)]));
        let held = gate.acquire(&Some("claude".to_string()));
        assert_eq!(held.as_deref(), Some("claude"));
        gate.release(held);
    }

    #[test]
    fn prestart_failures_leave_a_complete_resumable_artifact_set() {
        let dir = std::env::temp_dir().join(format!("sfh-prestart-{}", gen_uuid()));
        contain::mkdir_private(&dir).unwrap();
        let out = dir.join("leaf.out.txt");
        let err = dir.join("leaf.err.txt");
        let chain = dir.join("leaf.chain.txt");

        let persistence_error =
            persist_prestart_failure(&out, &err, &chain, "deadline exhausted before spawn");
        assert_eq!(persistence_error, None);
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "");
        assert_eq!(
            std::fs::read_to_string(&err).unwrap(),
            "deadline exhausted before spawn"
        );
        assert_eq!(std::fs::read_to_string(&chain).unwrap(), "");

        let _ = std::fs::remove_dir_all(&dir);
    }

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

        let oversized = concat!(
            r#"{"type":"message_end","message":{"role":"assistant","stopReason":"toolUse","content":[],"usage":{"input":18446744073709551615,"output":18446744073709551615}}}"#,
            "\n",
            r#"{"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"done"}],"usage":{"input":1,"output":1}}}"#
        );
        let oversized = parse_pi_jsonl(oversized);
        assert_eq!(oversized.usage.input_tokens, Some(u64::MAX));
        assert_eq!(oversized.usage.output_tokens, Some(u64::MAX));
    }

    #[test]
    fn pi_streaming_semantics_survive_more_than_raw_capture_limit() {
        let observer = PiStreamObserver::default();
        observer.observe(
            br#"{"type":"session","id":"stream-session","timestamp":"marker"}
{"type":"message_end","message":{"role":"assistant","stopReason":"toolUse","content":[],"usage":{"input":10,"output":2,"cost":{"total":0.25}}}}
"#,
        );

        let payload = "x".repeat(65_000);
        let noisy =
            format!("{{\"type\":\"message_update\",\"partial\":{{\"payload\":\"{payload}\"}}}}\n");
        let mut emitted = 0usize;
        while emitted <= 32 * 1024 * 1024 {
            observer.observe(noisy.as_bytes());
            emitted = emitted.saturating_add(noisy.len());
        }

        let final_record = br#"{"type":"message_end","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"VERDICT: PASS"}],"usage":{"input":20,"output":3,"cost":{"total":0.5}}}}"#;
        observer.observe(&final_record[..37]);
        observer.observe(&final_record[37..]);
        let (parsed, diagnostic) = observer.finish();

        assert_eq!(diagnostic, None);
        assert_eq!(parsed.text, "VERDICT: PASS");
        assert_eq!(parsed.session.as_deref(), Some("stream-session"));
        assert_eq!(parsed.session_marker.as_deref(), Some("marker"));
        assert_eq!(parsed.usage.input_tokens, Some(30));
        assert_eq!(parsed.usage.output_tokens, Some(5));
        assert_eq!(parsed.usage.cost_usd, Some(0.75));
        assert!(!parsed.failed);
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
        assert!(o.evidence.certifies_success());
        // A model failure prints no envelope at all.
        assert!(parse_cursor_json("Error: something went wrong").failed);
        // Since v1.2 an empty stdout is a MISSING protocol, not an empty final
        // message: `allow_empty` says a finished turn may answer with nothing,
        // not that a turn that never reported finishing counts as one.
        let empty = parse_cursor_json("");
        assert!(empty.failed);
        assert_eq!(empty.evidence.protocol, ProtocolState::MissingTerminal);
        // Leading noise (e.g. the worktree banner) must not hide the envelope.
        let noisy = format!("Using worktree: C:\\tmp\n{s}");
        assert_eq!(parse_cursor_json(&noisy).text, "hi there");
        assert!(parse_cursor_json(&noisy).evidence.certifies_success());
        // An arbitrary trailing JSON object is not a result envelope.
        let not_a_result = r#"{"type":"progress","result":"looks like an answer"}"#;
        assert!(parse_cursor_json(not_a_result).failed);
        assert!(parse_cursor_json(not_a_result).text.is_empty());
    }

    /// P0-01. Each of these is a real way agy ends a run, and every one of them
    /// used to be able to reach the execution layer's agy-only "non-empty text
    /// and nothing said it failed, so call the non-zero exit a success"
    /// correction. Only the last shape may certify anything.
    #[test]
    fn agy_only_certifies_success_from_a_recognised_terminal_record() {
        let cases: [(&str, &str); 5] = [
            (
                "plain usage error on stdout",
                "agy: unknown flag --output-format\nUsage: agy [options]",
            ),
            ("malformed JSON", r#"{"response":"looks fine","status":"SU"#),
            (
                "unknown status",
                r#"{"response":"looks fine","status":"WEIRD"}"#,
            ),
            (
                "no terminal record",
                r#"{"event":"progress","result":{"response":"partial"}}"#,
            ),
            ("empty output", ""),
        ];
        for (what, stdout) in cases {
            let o = parse_agy_json(stdout);
            assert!(
                !o.evidence.certifies_success(),
                "{what} must never excuse a non-zero exit"
            );
            assert!(o.failed, "{what} must be an in-band failure");
            assert!(
                o.text.is_empty(),
                "{what} must not hand raw stdout on as the answer (got {:?})",
                o.text
            );
        }
        // A documented terminal error is a failure that is nonetheless a
        // COMPLETE protocol - sfh knows exactly what happened.
        let err = parse_agy_json(r#"{"response":"nope","status":"ERROR"}"#);
        assert!(err.failed);
        assert_eq!(err.evidence.protocol, ProtocolState::Valid);
        assert!(err.evidence.terminal_seen);
        assert_eq!(err.evidence.terminal_success, Some(false));
        // The one shape that may correct a non-zero exit.
        let ok = parse_agy_json(
            r#"{"response":"the answer","status":"SUCCESS","conversation_id":"a-1","usage":{"input_tokens":3,"output_tokens":4}}"#,
        );
        assert!(ok.evidence.certifies_success());
        assert_eq!(ok.text, "the answer");
        assert_eq!(ok.session.as_deref(), Some("a-1"));
        assert_eq!(ok.usage.input_tokens, Some(3));
        // stream-json wrapping reaches the same record.
        let wrapped = parse_agy_json(
            r#"{"event":"result","result":{"response":"the answer","status":"SUCCESS"}}"#,
        );
        assert!(wrapped.evidence.certifies_success());
        assert_eq!(wrapped.text, "the answer");
    }

    /// P0-02. No preset parser may promote raw stdout to a final answer, and a
    /// stream that stops before its terminal record is never a success.
    #[test]
    fn every_preset_parser_fails_closed_without_its_terminal_record() {
        let noise = "I am not JSON at all, but I look like a helpful answer.";
        let checks: Vec<(&str, ParsedOut)> = vec![
            ("codex", parse_codex_jsonl(noise)),
            ("claude", parse_claude_json(noise)),
            ("opencode", parse_opencode_ndjson(noise)),
            ("grok", parse_grok_json(noise)),
            ("agy", parse_agy_json(noise)),
            ("pi", parse_pi_jsonl(noise)),
            ("cursor", parse_cursor_json(noise)),
        ];
        for (tool, o) in checks {
            assert!(
                !o.evidence.allows_success(),
                "{tool} accepted raw text as a completed protocol"
            );
            assert!(
                o.text.is_empty(),
                "{tool} handed raw stdout on as the answer: {:?}",
                o.text
            );
            assert!(
                o.evidence.failure_reason(tool).is_some(),
                "{tool} must explain why it failed"
            );
        }
        // Well-formed records, terminal record missing: a different failure
        // with the same fail-closed outcome.
        let truncated_codex = concat!(
            r#"{"type":"thread.started","thread_id":"t-1"}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"partial"}}"#,
        );
        let o = parse_codex_jsonl(truncated_codex);
        assert_eq!(o.evidence.protocol, ProtocolState::MissingTerminal);
        assert!(!o.evidence.allows_success());
        // A malformed record inside an otherwise fine JSONL stream invalidates
        // it: sfh cannot say what it missed.
        let broken = concat!(
            r#"{"type":"thread.started","thread_id":"t-1"}"#,
            "\n",
            "{oops",
            "\n",
            r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}"#,
        );
        let o = parse_codex_jsonl(broken);
        assert_eq!(o.evidence.protocol, ProtocolState::Invalid);
        assert_eq!(o.evidence.malformed_records, 1);
        assert!(!o.evidence.allows_success());
        // claude: a JSON envelope that is not the documented result record.
        let o = parse_claude_json(r#"{"type":"system","result":"not the answer"}"#);
        assert_eq!(o.evidence.protocol, ProtocolState::MissingTerminal);
        assert!(o.text.is_empty());
        // opencode: text parts but no finish/error event.
        let o = parse_opencode_ndjson(
            r#"{"type":"text","sessionID":"s","part":{"id":"p1","messageID":"m1","text":"hi"}}"#,
        );
        assert_eq!(o.evidence.protocol, ProtocolState::MissingTerminal);
        assert!(!o.evidence.allows_success());
        // pi: a session header and nothing else.
        let o = parse_pi_jsonl(r#"{"type":"session","id":"s-1","timestamp":"2026-01-01"}"#);
        assert_eq!(o.evidence.protocol, ProtocolState::MissingTerminal);
        assert!(!o.evidence.allows_success());
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
            workspace: None,
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
            run_clock: None,
            wall_deadline: None,
            budget: BudgetVars::default(),
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
        // -CommandWithArgs takes one command string; the rest become $args
        // inside it, exactly like the arguments after `sh -c SCRIPT`.
        assert_eq!(
            s(&["pwsh", "-CommandWithArgs", "Write-Output $args[0]", "data"]),
            Some(2..3)
        );
        assert_eq!(s(&["pwsh", "-cwa", "x", "a", "b"]), Some(2..3));
        // Any unambiguous prefix of the longer name resolves to it in the
        // shell, so it has to here as well - these used to match no branch at
        // all and let the script text through as ordinary argv data.
        assert_eq!(s(&["pwsh", "-CommandW", "x", "a"]), Some(2..3));
        assert_eq!(s(&["pwsh", "-commandwi", "x", "a"]), Some(2..3));
        assert_eq!(s(&["pwsh", "-CommandWithArg", "x", "a"]), Some(2..3));
        // PowerShell resolves -c, -com and the exact -Command to -Command,
        // never to -CommandWithArgs, so those still take the whole tail.
        assert_eq!(s(&["pwsh", "-c", "echo", "a"]), Some(2..4));
        assert_eq!(s(&["pwsh", "-com", "echo", "a"]), Some(2..4));
        assert_eq!(s(&["pwsh", "-Command", "echo", "a"]), Some(2..4));
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
        // -File hands everything after it to the script, so a -Command sitting
        // there is one of its arguments and is never re-parsed as code.
        assert_eq!(s(&["pwsh", "-File", "s.ps1", "-Command", "x"]), None);
        assert_eq!(s(&["pwsh", "-f", "s.ps1", "-c", "x"]), None);
        // The first bare word is a file too, with the same consequence.
        assert_eq!(s(&["pwsh", "s.ps1", "-Command", "x"]), None);
        // ...but -Command first wins, and everything after it - including
        // something that looks like -File - is part of the command line it
        // re-joins.
        assert_eq!(s(&["pwsh", "-Command", "x", "-File", "s.ps1"]), Some(2..5));

        // A templated SWITCH cannot be classified before it is rendered, so
        // everything from it on is treated as shell text and any template in
        // there is refused. Without this, ["pwsh","{{x}}","code {{y}}"] read as
        // a bare word and the code went through as ordinary data.
        assert_eq!(s(&["pwsh", "{{steps.a.output}}", "code"]), Some(1..3));
        assert_eq!(s(&["sh", "{{steps.a.output}}", "-c", "x"]), Some(1..4));
        // A template AFTER the script flag has already been classified, so the
        // positional-argument form is untouched.
        assert_eq!(
            s(&["sh", "-c", "cat \"$1\"", "n", "{{steps.a.output}}"]),
            Some(2..3)
        );
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
            workspace: None,
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
            run_clock: None,
            wall_deadline: None,
            budget: BudgetVars::default(),
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
    fn budget_vars_measure_against_the_ceiling_and_never_go_negative() {
        let f: flow::Flow = serde_yaml_ng::from_str(
            "defaults:\n  max_cost_usd: 2.0\nsteps:\n  - id: a\n    cmd: [\"echo\"]\n",
        )
        .expect("test flow parses");
        let b = BudgetVars::new(&f.defaults, 0.5, 10);
        assert_eq!(b.spent_usd, 0.5);
        assert_eq!(b.elapsed_sec, 10);
        // Measured against max_cost_usd, NOT against the on_budget threshold: a
        // landing step asking what is left means what is really left.
        assert_eq!(b.remaining_usd, Some(1.5));
        // No wall_clock_sec declared, so that axis has no remainder to report -
        // which make_builtins spells `unlimited` rather than 0.
        assert_eq!(b.remaining_sec, None);
        // A single step can overshoot the ceiling before the next check sees
        // it. "How much budget is left" is then none, not a negative amount.
        assert_eq!(
            BudgetVars::new(&f.defaults, 3.0, 10).remaining_usd,
            Some(0.0)
        );
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
