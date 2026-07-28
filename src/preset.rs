//! Preset command builders for the supported AI CLIs.
//!
//! Grounded in live-verified research (2026-07-27) against:
//!   codex-cli 0.146.0-alpha.3.1, claude 2.1.220, opencode 1.18.3,
//!   grok 0.2.112, agy 1.0.8.
//!
//! Every preset runs in the tool's machine-readable output mode, which gives
//! three things at once: a clean final message, the session id, and token/cost
//! usage. Key per-tool facts encoded here:
//! - codex: prompt on stdin via '-'; final text from --output-last-message;
//!   `--json` stdout carries thread.started/turn.completed; `exec resume` has
//!   no -s flag, so the sandbox is re-specified with -c sandbox_mode=...
//! - claude: prompt on stdin; --output-format json gives .result/.session_id/
//!   .total_cost_usd; plan mode is only advisory, so read = dontAsk + --tools.
//! - opencode: prompt on stdin; --auto is mandatory headless (an "ask" hangs
//!   forever); --format json emits NDJSON with sessionID on every line.
//! - grok: prompt ONLY via --prompt-file (stdin opens the TUI and hangs);
//!   --output-format json gives .text/.sessionId; read = dontAsk + --deny.
//! - agy: prompt ONLY as the value of a trailing -p; --output-format json gives
//!   .response/.status/.conversation_id; --print-timeout defaults to 5m.
//! - pi: prompt on stdin; --mode json emits JSONL (session header, then
//!   message_end events); --session-id both creates and resumes; there is no
//!   sandbox at all, so access levels are expressed as a --tools allowlist.
//! - cursor: prompt on stdin, but ONLY when no positional prompt is given;
//!   --output-format json returns one .result envelope; headless permissions are
//!   binary (deny-all without --force, approve-all with it); --resume creates or
//!   resumes, so a resume is only trustworthy once sfh has checked that the
//!   chat's store.db still exists in the same directory bucket.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Access {
    Read,
    Write,
    Full,
}

impl Access {
    /// `None` still parses to Write so cmd: steps (which never use it) need no
    /// special case; validation makes access mandatory for AI steps, so a
    /// preset step can only reach here with an explicitly declared level.
    pub fn parse(s: Option<&str>) -> Result<Access, String> {
        match s.unwrap_or("write") {
            "read" => Ok(Access::Read),
            "write" => Ok(Access::Write),
            "full" => Ok(Access::Full),
            other => Err(format!("access must be read/write/full, got '{other}'")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
            Access::Full => "full",
        }
    }

    /// Privilege ordering, used to refuse escalating a resumed session
    /// (read < write < full).
    pub fn rank(self) -> u8 {
        match self {
            Access::Read => 0,
            Access::Write => 1,
            Access::Full => 2,
        }
    }
}

/// How to turn the raw process output into text + session id + usage.
#[derive(Clone)]
pub enum OutputParse {
    /// Plain stdout (custom cmd: steps).
    Stdout,
    /// codex: text from the --output-last-message file, metadata from JSONL stdout.
    CodexJsonl(PathBuf),
    /// claude --output-format json: single-line envelope.
    ClaudeJson,
    /// opencode --format json: NDJSON event stream.
    OpencodeNdjson,
    /// grok --output-format json: one pretty-printed object.
    GrokJson,
    /// agy --output-format json: single-line envelope.
    AgyJson,
    /// pi --mode json: JSONL events (session header + message_end per turn).
    PiJsonl,
    /// cursor-agent --output-format json: single-line result envelope.
    CursorJson,
}

/// How the rendered prompt reaches the tool.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Delivery {
    Stdin,
    /// Passed via a file flag (grok --prompt-file); nothing on stdin.
    PromptFile,
    /// Appended as the final argv element, adjacent to a preceding value-taking
    /// flag (agy -p <prompt>). Size-capped by the caller.
    Arg,
    None,
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

pub struct Built {
    pub argv: Vec<String>,
    pub parse: OutputParse,
    pub delivery: Delivery,
    /// Session id sfh assigned itself (claude/grok); otherwise parsed from output.
    pub preassigned_session: Option<String>,
    /// Env vars to remove from the child (claude: nested-session vars).
    pub env_remove: Vec<String>,
    /// Env vars to set on the child (opencode: permission hardening).
    pub env_set: Vec<(String, String)>,
    /// When resuming: the session id the tool MUST echo back (agy silently
    /// starts a new conversation on an unknown id - detect that as failure).
    pub expect_session: Option<String>,
    /// When resuming: the session marker the tool must report back. Filled in
    /// by the caller from the recorded session (pi: creation timestamp).
    pub expect_marker: Option<String>,
    pub warnings: Vec<String>,
}

pub struct PresetInput<'a> {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub access: Access,
    pub agent: Option<String>,
    pub extra: &'a [String],
    /// Replaces the preset's default executable name (argv[0]).
    pub bin: Option<String>,
    /// Step timeout; agy needs it as a CLI flag (--print-timeout), not just a kill timer.
    pub timeout_sec: Option<u64>,
}

pub struct BuildPaths<'a> {
    pub last_msg: &'a Path,
    pub prompt_file: &'a Path,
}

/// True when this tool lets sfh choose the session id up front.
pub fn wants_preassign(tool: &str) -> bool {
    matches!(tool, "claude" | "grok" | "pi" | "cursor")
}

/// Tools where a resume MUST run in the same directory as the original, because
/// a lookup miss silently creates a fresh session instead of failing. For these
/// sfh refuses rather than warns.
pub fn resume_requires_same_cwd(tool: &str) -> bool {
    tool == "cursor"
}

/// Where cursor keeps a chat: <config>/chats/<hash of cwd>/<chat-id>/store.db.
/// sfh records the resolved path when a chat is created and re-checks it before
/// resuming: `--resume <unknown id>` silently CREATES a chat and echoes the id
/// back, so the store's existence is the only proof a resume is real.
pub fn cursor_chat_store(session_id: &str) -> Option<PathBuf> {
    let root = std::env::var_os("CURSOR_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(|x| PathBuf::from(x).join("cursor")))
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|h| PathBuf::from(h).join(".cursor"))
        })?
        .join("chats");
    for bucket in std::fs::read_dir(root).ok()?.flatten() {
        let db = bucket.path().join(session_id).join("store.db");
        if db.is_file() {
            return Some(db);
        }
    }
    None
}

/// Tools whose RESUME lookup is scoped to the working directory (verified live
/// for pi: the same --session-id in another cwd silently creates a new session).
pub fn session_is_cwd_scoped(tool: &str) -> bool {
    matches!(tool, "claude" | "grok" | "agy" | "pi" | "cursor")
}

/// Fork resolution is more tolerant than resume: pi's --fork looks up the id
/// project-locally then globally, and opencode session ids are global.
pub fn fork_is_cwd_scoped(tool: &str) -> bool {
    matches!(tool, "claude" | "grok")
}

/// Tools that can branch a session headlessly into a NEW independent session.
/// codex's `fork` is TUI-only and `exec resume` appends to the parent; agy has
/// no fork at all.
pub fn supports_fork(tool: &str) -> bool {
    matches!(tool, "claude" | "opencode" | "grok" | "pi")
}

/// The executable a preset launches when no `bin:` overrides it. Every preset
/// runs the tool's own name except cursor, whose CLI is `cursor-agent` -
/// probing plain `cursor` would start the Electron editor.
pub fn default_program(tool: &str) -> String {
    match tool {
        "cursor" => "cursor-agent".to_string(),
        t => t.to_string(),
    }
}

/// Forking pays off because the child's prompt prefix is byte-identical to the
/// parent's, so the provider's prompt cache can hit - but N children racing the
/// first cache write all miss it. Measured on claude: one warm-up child first
/// turned $0.0337 per child into $0.0026. Only claude showed a real saving.
pub fn fork_warmup_pays(tool: &str) -> bool {
    tool == "claude"
}

/// pi has no sandbox and no permission prompts: the only real lever is which
/// tools get registered. Bare `pi` already has read+bash+edit+write.
fn pi_tools(access: Access) -> &'static str {
    match access {
        Access::Read => "read,grep,find,ls",
        // Deliberately no bash: without a sandbox, a shell is indistinguishable
        // from full access.
        Access::Write => "read,edit,write,grep,find,ls",
        Access::Full => "read,bash,edit,write,grep,find,ls",
    }
}

fn pi_common(a: &mut Vec<String>, inp: &PresetInput, warnings: &mut Vec<String>) {
    if let Some(m) = &inp.model {
        push(a, &["--model"]);
        a.push(m.clone());
    }
    if let Some(e) = &inp.effort {
        push(a, &["--thinking"]);
        a.push(e.clone());
    }
    if inp.agent.is_some() {
        warnings.push(
            "pi preset ignores 'agent' (no --agent flag; use args: [\"--append-system-prompt\", \"...\"] for a persona)"
                .into(),
        );
    }
    push(a, &["--tools"]);
    a.push(pi_tools(inp.access).to_string());
    match inp.access {
        // Project-local extensions/skills are TypeScript that runs with full
        // process rights regardless of the tool allowlist, so anything below full
        // must refuse to load them for the allowlist to mean anything: an
        // extension in the repo could register Bash and undo the write tier.
        Access::Read => push(
            a,
            &[
                "--no-extensions",
                "--no-skills",
                "--no-prompt-templates",
                "--no-approve",
            ],
        ),
        Access::Write => {
            push(
                a,
                &[
                    "--no-extensions",
                    "--no-skills",
                    "--no-prompt-templates",
                    "--no-approve",
                ],
            );
            warnings.push(
                "pi write registers no shell tool (pi has no sandbox, so bash would equal full access); use access: full if the step must run commands (or args: [\"-t\", \"...\"] together with allow_access_override: true)"
                    .into(),
            );
        }
        Access::Full => push(a, &["--approve"]),
    }
}

/// model/effort/agent/access flags shared by claude's fresh, resume and fork paths.
fn claude_common(a: &mut Vec<String>, inp: &PresetInput, warnings: &mut Vec<String>) {
    if let Some(m) = &inp.model {
        push(a, &["--model"]);
        a.push(m.clone());
    }
    if let Some(e) = &inp.effort {
        push(a, &["--effort"]);
        a.push(e.clone());
    }
    if let Some(ag) = &inp.agent {
        push(a, &["--agent"]);
        a.push(ag.clone());
    }
    match inp.access {
        // plan mode is only advisory when bypass is available, so read is a
        // dontAsk + builtin-tool whitelist instead.
        Access::Read => push(
            a,
            &["--permission-mode", "dontAsk", "--tools", CLAUDE_READ_TOOLS],
        ),
        Access::Write => {
            push(
                a,
                &[
                    "--permission-mode",
                    "acceptEdits",
                    "--allowedTools",
                    CLAUDE_WRITE_ALLOWED,
                ],
            );
            warnings.push(
                "claude write auto-approves edits only; shell commands are not auto-approved (use access: full if the step must run commands, or args: [\"--allowedTools\", \"Bash,...\"] together with allow_access_override: true)".into(),
            );
        }
        Access::Full => push(a, &["--dangerously-skip-permissions"]),
    }
}

/// model/effort/agent/access flags shared by grok's fresh, resume and fork paths.
fn grok_common(a: &mut Vec<String>, inp: &PresetInput, warnings: &mut Vec<String>) {
    if let Some(m) = &inp.model {
        push(a, &["-m"]);
        a.push(m.clone());
    }
    if let Some(e) = &inp.effort {
        push(a, &["--reasoning-effort"]);
        a.push(e.clone());
    }
    if let Some(ag) = &inp.agent {
        push(a, &["--agent"]);
        a.push(ag.clone());
    }
    // --permission-mode plan is compat-only in headless and --sandbox is a no-op
    // on Windows, so read = dontAsk + hard deny rules (deny always wins).
    match inp.access {
        Access::Read => push(
            a,
            &[
                "--permission-mode",
                "dontAsk",
                "--deny",
                "Edit",
                "--deny",
                "Write",
                "--deny",
                "Bash",
            ],
        ),
        Access::Write => {
            push(a, &["--permission-mode", "acceptEdits"]);
            warnings.push(
                "grok write auto-approves edits only; shell commands are not auto-approved (use access: full if the step must run commands, or args: [\"--allow\", \"Bash(...)\"] together with allow_access_override: true)".into(),
            );
        }
        Access::Full => push(a, &["--permission-mode", "bypassPermissions"]),
    }
}

const CLAUDE_ENV_SCRUB: [&str; 8] = [
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_HOST_SESSION_ID",
    "ANTHROPIC_MODEL",
];

/// claude read-level: hard guarantee via dontAsk + builtin-tool whitelist
/// (plan mode is only a soft instruction when bypass is available).
const CLAUDE_READ_TOOLS: &str = "Read,Glob,Grep,WebSearch,WebFetch,TodoWrite";
/// claude write-level: acceptEdits auto-approves fs edits; web tools need an
/// explicit allow or a -p run aborts on first denial.
///
/// Bash is deliberately NOT here. claude has no OS sandbox, so an auto-approved
/// shell at the "write" tier would be indistinguishable from `full` - the same
/// reason pi's write tier registers no shell (see `pi_tools`). Steps that really
/// need commands say so with `args:`, or use `access: full` and mean it.
const CLAUDE_WRITE_ALLOWED: &str = "WebSearch,WebFetch";

/// Flags a user could put in `args:` that escalate past `access:` regardless of
/// the tool. Flag-shaped entries match a whole argv element or its `--flag=...`
/// form; bare values (permission-mode names) match as substrings.
pub const GLOBAL_ESCALATION_FLAGS: [&str; 7] = [
    "--dangerously-skip-permissions",
    "--dangerously-bypass-approvals-and-sandbox",
    "bypassPermissions",
    "danger-full-access",
    "--always-approve",
    "--yolo",
    "--force",
];

/// Per-tool `args:` that change what the tool is allowed to do. The preset
/// itself emits several of these (pi --tools, claude --permission-mode, ...);
/// the check applies to USER args only, never to the argv sfh builds.
/// Entries ending in '=' match a value namespace (codex -c sandbox_mode=...).
pub fn escalation_flags(tool: &str) -> &'static [&'static str] {
    match tool {
        "codex" => &["-s", "--sandbox", "sandbox_mode=", "approval_policy="],
        "claude" => &[
            "--tools",
            "--allowedTools",
            "--permission-mode",
            "--add-dir",
        ],
        "opencode" => &["--agent"],
        "grok" => &["--allow", "--permission-mode"],
        "agy" => &["--mode"],
        "pi" => &["--approve", "--tools", "-t"],
        _ => &[],
    }
}

fn escalation_pattern_matches(pattern: &str, arg: &str) -> bool {
    if pattern.ends_with('=') {
        arg.starts_with(pattern)
    } else if pattern.starts_with('-') {
        arg == pattern || arg.starts_with(&format!("{pattern}="))
    } else {
        arg.contains(pattern)
    }
}

/// True when one user-supplied arg would override the declared access level.
/// A step that is not `full` may carry such an arg only with an explicit
/// allow_access_override: true (enforced by the caller, fail-closed).
pub fn is_escalation_arg(tool: &str, arg: &str) -> bool {
    GLOBAL_ESCALATION_FLAGS
        .iter()
        .chain(escalation_flags(tool).iter())
        .any(|p| escalation_pattern_matches(p, arg))
}

fn push(a: &mut Vec<String>, items: &[&str]) {
    a.extend(items.iter().map(|s| s.to_string()));
}

fn agy_model_has_effort_suffix(model: Option<&str>) -> bool {
    model
        .map(|m| {
            ["-low", "-medium", "-high", "-thinking"]
                .iter()
                .any(|s| m.ends_with(s))
        })
        .unwrap_or(false)
}

fn opencode_agent(a: &mut Vec<String>, inp: &PresetInput) -> String {
    match (&inp.agent, inp.access) {
        (Some(ag), _) => {
            push(a, &["--agent"]);
            a.push(ag.clone());
            ag.clone()
        }
        (None, Access::Read) => {
            push(a, &["--agent", "plan"]);
            "plan".to_string()
        }
        (None, _) => {
            // Pin the agent so write/full don't inherit the user's default.
            push(a, &["--agent", "build"]);
            "build".to_string()
        }
    }
}

fn opencode_env(agent_name: &str, access: Access) -> Vec<(String, String)> {
    match access {
        // The stock plan agent denies edits but NOT bash (1.18.3), so a read step
        // could still write via shell redirection. OPENCODE_CONFIG_CONTENT is the
        // highest-precedence config layer and merges with the user's config.
        Access::Read => vec![(
            "OPENCODE_CONFIG_CONTENT".to_string(),
            format!(
                "{{\"agent\":{{\"{agent_name}\":{{\"permission\":{{\"edit\":\"deny\",\"bash\":\"deny\"}}}}}}}}"
            ),
        )],
        // --auto auto-approves whatever is not explicitly denied, so write denies
        // two things: bash (opencode has no OS sandbox - an auto-approved shell
        // would make write == full, the same rule pi/claude/grok follow) and
        // out-of-tree writes. Agent-scoped so a user-selected agent cannot
        // inherit looser defaults.
        Access::Write => vec![(
            "OPENCODE_CONFIG_CONTENT".to_string(),
            format!(
                "{{\"agent\":{{\"{agent_name}\":{{\"permission\":{{\"bash\":\"deny\"}}}}}},\"permission\":{{\"external_directory\":\"deny\"}}}}"
            ),
        )],
        Access::Full => Vec::new(),
    }
}

/// Build the command line for a fresh (non-resumed) preset run.
pub fn build(
    tool: &str,
    inp: PresetInput,
    paths: &BuildPaths,
    preassign_session: Option<&str>,
) -> Result<Built, String> {
    let mut a: Vec<String> = Vec::new();
    let mut warnings = Vec::new();
    let mut env_remove = Vec::new();
    let mut env_set = Vec::new();
    let parse;
    let delivery;
    let mut preassigned = None;
    match tool {
        "codex" => {
            push(
                &mut a,
                &[
                    "codex",
                    "exec",
                    "--skip-git-repo-check",
                    "--color",
                    "never",
                    "--json",
                ],
            );
            // The user's config may set approval_policy loosely; pin it for headless.
            push(&mut a, &["-c", "approval_policy=\"never\""]);
            if let Some(m) = &inp.model {
                push(&mut a, &["-m"]);
                a.push(m.clone());
            }
            if let Some(e) = &inp.effort {
                push(&mut a, &["-c"]);
                a.push(format!("model_reasoning_effort=\"{e}\""));
            }
            // Always pass -s explicitly: a global config may default to danger-full-access.
            match inp.access {
                Access::Read => push(&mut a, &["-s", "read-only"]),
                Access::Write => push(&mut a, &["-s", "workspace-write"]),
                Access::Full => push(&mut a, &["--dangerously-bypass-approvals-and-sandbox"]),
            }
            if inp.agent.is_some() {
                warnings.push("codex preset ignores 'agent' (no --agent flag in exec)".into());
            }
            a.extend(inp.extra.iter().cloned());
            push(&mut a, &["--output-last-message"]);
            a.push(paths.last_msg.display().to_string());
            push(&mut a, &["-"]);
            parse = OutputParse::CodexJsonl(paths.last_msg.to_path_buf());
            delivery = Delivery::Stdin;
        }
        "claude" => {
            push(&mut a, &["claude", "-p", "--output-format", "json"]);
            claude_common(&mut a, &inp, &mut warnings);
            if let Some(id) = preassign_session {
                push(&mut a, &["--session-id"]);
                a.push(id.to_string());
                preassigned = Some(id.to_string());
            }
            a.extend(inp.extra.iter().cloned());
            env_remove = CLAUDE_ENV_SCRUB.iter().map(|s| s.to_string()).collect();
            parse = OutputParse::ClaudeJson;
            delivery = Delivery::Stdin;
        }
        "opencode" => {
            push(&mut a, &["opencode", "run", "--format", "json"]);
            if let Some(m) = &inp.model {
                push(&mut a, &["-m"]);
                a.push(m.clone());
            }
            if let Some(e) = &inp.effort {
                push(&mut a, &["--variant"]);
                a.push(e.clone());
            }
            let agent_name = opencode_agent(&mut a, &inp);
            // --auto is mandatory headless: any permission that resolves to "ask"
            // has no UI in `opencode run` and hangs forever. Explicit deny rules
            // are still enforced with --auto.
            push(&mut a, &["--auto"]);
            env_set = opencode_env(&agent_name, inp.access);
            a.extend(inp.extra.iter().cloned());
            parse = OutputParse::OpencodeNdjson;
            delivery = Delivery::Stdin;
        }
        "grok" => {
            push(&mut a, &["grok", "--output-format", "json"]);
            grok_common(&mut a, &inp, &mut warnings);
            if let Some(id) = preassign_session {
                push(&mut a, &["--session-id"]);
                a.push(id.to_string());
                preassigned = Some(id.to_string());
            }
            a.extend(inp.extra.iter().cloned());
            push(&mut a, &["--prompt-file"]);
            a.push(paths.prompt_file.display().to_string());
            parse = OutputParse::GrokJson;
            delivery = Delivery::PromptFile;
        }
        "agy" => {
            push(&mut a, &["agy"]);
            if let Some(m) = &inp.model {
                push(&mut a, &["--model"]);
                a.push(m.clone());
            }
            if let Some(e) = &inp.effort {
                // Valid: low|medium|high, but agy hard-errors when --effort is
                // combined with a model id that already encodes effort.
                if agy_model_has_effort_suffix(inp.model.as_deref()) {
                    warnings.push(format!(
                        "agy: model id already encodes the effort level; ignoring effort '{e}'"
                    ));
                } else {
                    push(&mut a, &["--effort"]);
                    a.push(e.clone());
                }
            }
            if let Some(ag) = &inp.agent {
                push(&mut a, &["--agent"]);
                a.push(ag.clone());
            }
            // agy's --print-timeout defaults to 5m0s and kills longer runs.
            let t = inp.timeout_sec.unwrap_or(3600);
            push(&mut a, &["--print-timeout"]);
            a.push(format!("{t}s"));
            match inp.access {
                Access::Read => push(&mut a, &["--mode", "plan"]),
                Access::Write => push(&mut a, &["--mode", "accept-edits"]),
                Access::Full => push(&mut a, &["--dangerously-skip-permissions"]),
            }
            a.extend(inp.extra.iter().cloned());
            // The envelope's status field corrects agy's unreliable exit codes.
            push(&mut a, &["--output-format", "json"]);
            // -p is a Go string flag: the prompt MUST be the next argv element.
            push(&mut a, &["-p"]);
            parse = OutputParse::AgyJson;
            delivery = Delivery::Arg;
        }
        "cursor" => {
            // --trust: headless refuses to run in an untrusted directory.
            // --disable-project-configs: a repo-supplied .cursor/cli.json can set
            // approvalMode "unrestricted", which silently equals --force.
            // --disable-auto-update: the launcher can otherwise swap the binary
            // mid-flow by piping an installer into PowerShell.
            push(
                &mut a,
                &[
                    "cursor-agent",
                    "-p",
                    "--output-format",
                    "json",
                    "--trust",
                    "--disable-auto-update",
                    "--disable-project-configs",
                ],
            );
            if let Some(m) = &inp.model {
                push(&mut a, &["--model"]);
                a.push(m.clone());
            }
            if inp.effort.is_some() {
                warnings.push(
                    "cursor preset ignores 'effort' (encode it in the model id, e.g. sonnet-4-thinking)"
                        .into(),
                );
            }
            if inp.agent.is_some() {
                warnings.push("cursor preset ignores 'agent' (no --agent flag)".into());
            }
            match inp.access {
                // Headless has exactly two tiers: without --force every gated
                // operation is denied outright; with it, everything is approved
                // (shell included). A "write" tier cannot exist, so asking for
                // one is an error instead of a silent full (validation rejects
                // it too; this is the fail-closed backstop).
                Access::Read => push(&mut a, &["--mode", "plan"]),
                Access::Write => {
                    return Err(
                        "cursor headless has only two permission tiers: read (deny-all) and full (--force, approve-all); access: write is not supported - pick read or full"
                            .into(),
                    )
                }
                Access::Full => push(&mut a, &["--force"]),
            }
            // --resume both creates and resumes, so naming the chat up front is
            // what makes it findable later.
            if let Some(id) = preassign_session {
                push(&mut a, &["--resume"]);
                a.push(id.to_string());
                preassigned = Some(id.to_string());
            }
            a.extend(inp.extra.iter().cloned());
            parse = OutputParse::CursorJson;
            delivery = Delivery::Stdin;
        }
        "pi" => {
            // --mode json already forces non-interactive; -p is avoided because
            // it swallows the next argv token.
            push(&mut a, &["pi", "--mode", "json", "--offline"]);
            pi_common(&mut a, &inp, &mut warnings);
            if let Some(id) = preassign_session {
                push(&mut a, &["--session-id"]);
                a.push(id.to_string());
                preassigned = Some(id.to_string());
            }
            a.extend(inp.extra.iter().cloned());
            parse = OutputParse::PiJsonl;
            delivery = Delivery::Stdin;
        }
        other => {
            return Err(format!(
                "unknown tool '{other}' (use {}, or a custom cmd:)",
                crate::flow::TOOLS.join("/")
            ))
        }
    }
    if let Some(bin) = inp.bin {
        a[0] = bin;
    }
    Ok(Built {
        argv: a,
        parse,
        delivery,
        preassigned_session: preassigned,
        env_remove,
        env_set,
        expect_session: None,
        expect_marker: None,
        warnings,
    })
}

/// Build the command line to RESUME a previous session with a new prompt.
pub fn build_resume(
    tool: &str,
    session_id: &str,
    inp: PresetInput,
    paths: &BuildPaths,
) -> Result<Built, String> {
    let mut a: Vec<String> = Vec::new();
    let mut warnings = Vec::new();
    let mut env_remove = Vec::new();
    let mut env_set = Vec::new();
    let parse;
    let delivery;
    let mut expect_session = None;
    match tool {
        "codex" => {
            push(&mut a, &["codex", "exec", "resume"]);
            a.push(session_id.to_string());
            push(
                &mut a,
                &[
                    "--skip-git-repo-check",
                    "--json",
                    "-c",
                    "approval_policy=\"never\"",
                ],
            );
            if let Some(m) = &inp.model {
                push(&mut a, &["-m"]);
                a.push(m.clone());
            }
            if let Some(e) = &inp.effort {
                push(&mut a, &["-c"]);
                a.push(format!("model_reasoning_effort=\"{e}\""));
            }
            // `exec resume` has no -s flag and the sandbox level is NOT inherited
            // from the original session (live-verified) - rebuild it via -c.
            match inp.access {
                Access::Read => {
                    push(&mut a, &["-c"]);
                    a.push("sandbox_mode=\"read-only\"".into());
                }
                Access::Write => {
                    push(&mut a, &["-c"]);
                    a.push("sandbox_mode=\"workspace-write\"".into());
                }
                Access::Full => push(&mut a, &["--dangerously-bypass-approvals-and-sandbox"]),
            }
            a.extend(inp.extra.iter().cloned());
            push(&mut a, &["--output-last-message"]);
            a.push(paths.last_msg.display().to_string());
            push(&mut a, &["-"]);
            parse = OutputParse::CodexJsonl(paths.last_msg.to_path_buf());
            delivery = Delivery::Stdin;
        }
        "claude" => {
            push(&mut a, &["claude", "-p", "--output-format", "json", "-r"]);
            a.push(session_id.to_string());
            claude_common(&mut a, &inp, &mut warnings);
            a.extend(inp.extra.iter().cloned());
            env_remove = CLAUDE_ENV_SCRUB.iter().map(|s| s.to_string()).collect();
            expect_session = Some(session_id.to_string());
            parse = OutputParse::ClaudeJson;
            delivery = Delivery::Stdin;
        }
        "opencode" => {
            push(&mut a, &["opencode", "run", "--format", "json", "-s"]);
            a.push(session_id.to_string());
            if let Some(m) = &inp.model {
                push(&mut a, &["-m"]);
                a.push(m.clone());
            }
            if let Some(e) = &inp.effort {
                push(&mut a, &["--variant"]);
                a.push(e.clone());
            }
            let agent_name = opencode_agent(&mut a, &inp);
            push(&mut a, &["--auto"]);
            env_set = opencode_env(&agent_name, inp.access);
            a.extend(inp.extra.iter().cloned());
            expect_session = Some(session_id.to_string());
            parse = OutputParse::OpencodeNdjson;
            delivery = Delivery::Stdin;
        }
        "grok" => {
            push(&mut a, &["grok", "--output-format", "json", "--resume"]);
            a.push(session_id.to_string());
            grok_common(&mut a, &inp, &mut warnings);
            a.extend(inp.extra.iter().cloned());
            push(&mut a, &["--prompt-file"]);
            a.push(paths.prompt_file.display().to_string());
            expect_session = Some(session_id.to_string());
            parse = OutputParse::GrokJson;
            delivery = Delivery::PromptFile;
        }
        "agy" => {
            push(&mut a, &["agy", "--conversation"]);
            a.push(session_id.to_string());
            if let Some(m) = &inp.model {
                push(&mut a, &["--model"]);
                a.push(m.clone());
            }
            if let Some(e) = &inp.effort {
                if agy_model_has_effort_suffix(inp.model.as_deref()) {
                    warnings.push(format!(
                        "agy: model id already encodes the effort level; ignoring effort '{e}'"
                    ));
                } else {
                    push(&mut a, &["--effort"]);
                    a.push(e.clone());
                }
            }
            if let Some(ag) = &inp.agent {
                push(&mut a, &["--agent"]);
                a.push(ag.clone());
            }
            let t = inp.timeout_sec.unwrap_or(3600);
            push(&mut a, &["--print-timeout"]);
            a.push(format!("{t}s"));
            match inp.access {
                Access::Read => push(&mut a, &["--mode", "plan"]),
                Access::Write => push(&mut a, &["--mode", "accept-edits"]),
                Access::Full => push(&mut a, &["--dangerously-skip-permissions"]),
            }
            a.extend(inp.extra.iter().cloned());
            // agy silently starts a NEW conversation on an unknown id; only the
            // JSON envelope lets us detect that (expect_session).
            push(&mut a, &["--output-format", "json"]);
            expect_session = Some(session_id.to_string());
            push(&mut a, &["-p"]);
            parse = OutputParse::AgyJson;
            delivery = Delivery::Arg;
        }
        "pi" => {
            // --session-id creates OR resumes; the argv is identical to a fresh
            // run. The id therefore always matches, so the resume guard compares
            // the session header timestamp instead (see expect_marker).
            push(&mut a, &["pi", "--mode", "json", "--offline"]);
            pi_common(&mut a, &inp, &mut warnings);
            push(&mut a, &["--session-id"]);
            a.push(session_id.to_string());
            a.extend(inp.extra.iter().cloned());
            expect_session = Some(session_id.to_string());
            parse = OutputParse::PiJsonl;
            delivery = Delivery::Stdin;
        }
        "cursor" => {
            // Same argv as a fresh run: --resume creates or resumes. The caller
            // has already pre-flighted the chat store, because an unknown id
            // silently starts a NEW chat and echoes the id back either way.
            push(
                &mut a,
                &[
                    "cursor-agent",
                    "-p",
                    "--output-format",
                    "json",
                    "--trust",
                    "--disable-auto-update",
                    "--disable-project-configs",
                ],
            );
            if let Some(m) = &inp.model {
                push(&mut a, &["--model"]);
                a.push(m.clone());
            }
            match inp.access {
                Access::Read => push(&mut a, &["--mode", "plan"]),
                Access::Write => {
                    return Err(
                        "cursor headless has only two permission tiers: read (deny-all) and full (--force, approve-all); access: write is not supported - pick read or full"
                            .into(),
                    )
                }
                Access::Full => push(&mut a, &["--force"]),
            }
            push(&mut a, &["--resume"]);
            a.push(session_id.to_string());
            a.extend(inp.extra.iter().cloned());
            expect_session = Some(session_id.to_string());
            parse = OutputParse::CursorJson;
            delivery = Delivery::Stdin;
        }
        other => return Err(format!("tool '{other}' does not support continue_from")),
    }
    if let Some(bin) = inp.bin {
        a[0] = bin;
    }
    Ok(Built {
        argv: a,
        parse,
        delivery,
        preassigned_session: Some(session_id.to_string()),
        env_remove,
        env_set,
        expect_session,
        expect_marker: None,
        warnings,
    })
}

/// Build the command line to FORK a session: the child inherits the parent's
/// history but writes to its own session, so N children can diverge from one
/// context concurrently without corrupting each other or the parent.
///
/// All four supporting tools refuse loudly (exit 1, before any model call) when
/// the parent id does not exist, so - unlike pi's create-or-resume --session-id -
/// a fork cannot silently degrade into a cold session. The remaining risk is the
/// fork flag being ignored, which the caller detects by requiring child != parent.
pub fn build_fork(
    tool: &str,
    parent_session_id: &str,
    child_session_id: &str,
    inp: PresetInput,
    paths: &BuildPaths,
) -> Result<Built, String> {
    let mut a: Vec<String> = Vec::new();
    let mut warnings = Vec::new();
    let mut env_remove = Vec::new();
    let mut env_set = Vec::new();
    let parse;
    let delivery;
    // opencode mints the child id itself; the others let sfh name it.
    let mut preassigned = Some(child_session_id.to_string());
    match tool {
        "claude" => {
            push(&mut a, &["claude", "-p", "--output-format", "json", "-r"]);
            a.push(parent_session_id.to_string());
            push(&mut a, &["--fork-session", "--session-id"]);
            a.push(child_session_id.to_string());
            claude_common(&mut a, &inp, &mut warnings);
            a.extend(inp.extra.iter().cloned());
            env_remove = CLAUDE_ENV_SCRUB.iter().map(|s| s.to_string()).collect();
            parse = OutputParse::ClaudeJson;
            delivery = Delivery::Stdin;
        }
        "opencode" => {
            push(&mut a, &["opencode", "run", "--format", "json", "-s"]);
            a.push(parent_session_id.to_string());
            push(&mut a, &["--fork"]);
            if let Some(m) = &inp.model {
                push(&mut a, &["-m"]);
                a.push(m.clone());
            }
            if let Some(e) = &inp.effort {
                push(&mut a, &["--variant"]);
                a.push(e.clone());
            }
            let agent_name = opencode_agent(&mut a, &inp);
            push(&mut a, &["--auto"]);
            env_set = opencode_env(&agent_name, inp.access);
            a.extend(inp.extra.iter().cloned());
            preassigned = None; // the child id only exists in the output stream
            parse = OutputParse::OpencodeNdjson;
            delivery = Delivery::Stdin;
        }
        "grok" => {
            push(&mut a, &["grok", "--output-format", "json", "--resume"]);
            a.push(parent_session_id.to_string());
            push(&mut a, &["--fork-session", "--session-id"]);
            a.push(child_session_id.to_string());
            grok_common(&mut a, &inp, &mut warnings);
            a.extend(inp.extra.iter().cloned());
            push(&mut a, &["--prompt-file"]);
            a.push(paths.prompt_file.display().to_string());
            parse = OutputParse::GrokJson;
            delivery = Delivery::PromptFile;
        }
        "pi" => {
            push(&mut a, &["pi", "--mode", "json", "--offline", "--fork"]);
            a.push(parent_session_id.to_string());
            pi_common(&mut a, &inp, &mut warnings);
            push(&mut a, &["--session-id"]);
            a.push(child_session_id.to_string());
            a.extend(inp.extra.iter().cloned());
            parse = OutputParse::PiJsonl;
            delivery = Delivery::Stdin;
        }
        other => {
            return Err(format!(
                "tool '{other}' cannot fork a session headlessly (only {}); use continue_from to chain serially, or give this step its own context",
                ["claude", "opencode", "grok", "pi"].join("/")
            ))
        }
    }
    if let Some(bin) = inp.bin {
        a[0] = bin;
    }
    Ok(Built {
        argv: a,
        parse,
        delivery,
        preassigned_session: preassigned,
        env_remove,
        env_set,
        // A fork must NOT come back as the parent - that would mean the fork flag
        // was ignored and this run appended to the shared parent instead.
        expect_session: None,
        expect_marker: None,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn inp<'a>(access: Access, extra: &'a [String]) -> PresetInput<'a> {
        PresetInput {
            model: None,
            effort: None,
            access,
            agent: None,
            extra,
            bin: None,
            timeout_sec: Some(900),
        }
    }

    fn paths() -> (PathBuf, PathBuf) {
        (PathBuf::from("/tmp/last.txt"), PathBuf::from("/tmp/p.txt"))
    }

    fn build_argv(tool: &str, access: Access) -> Vec<String> {
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        build(tool, inp(access, &[]), &bp, None).unwrap().argv
    }

    #[test]
    fn agy_prompt_flag_is_last_so_delivery_binds_it() {
        let argv = build_argv("agy", Access::Read);
        assert_eq!(
            argv.last().unwrap(),
            "-p",
            "prompt must attach to -p as its value"
        );
        assert!(argv
            .windows(2)
            .any(|w| w[0] == "--print-timeout" && w[1] == "900s"));
        assert!(argv.windows(2).any(|w| w[0] == "--mode" && w[1] == "plan"));
    }

    #[test]
    fn grok_uses_prompt_file_never_stdin() {
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        let b = build("grok", inp(Access::Read, &[]), &bp, None).unwrap();
        assert_eq!(b.delivery, Delivery::PromptFile);
        assert_eq!(b.argv.last().unwrap(), &p.display().to_string());
        assert!(b.argv.iter().any(|x| x == "dontAsk"));
        assert_eq!(b.argv.iter().filter(|x| *x == "--deny").count(), 3);
    }

    #[test]
    fn codex_always_pins_sandbox_and_ends_with_stdin_marker() {
        for (acc, needle) in [
            (Access::Read, "read-only"),
            (Access::Write, "workspace-write"),
            (Access::Full, "--dangerously-bypass-approvals-and-sandbox"),
        ] {
            let argv = build_argv("codex", acc);
            assert!(argv.iter().any(|x| x == needle), "{needle} missing");
            assert_eq!(
                argv.last().unwrap(),
                "-",
                "codex reads the prompt from stdin"
            );
        }
    }

    #[test]
    fn codex_resume_rebuilds_sandbox_via_config_override() {
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        let b = build_resume("codex", "sess-1", inp(Access::Read, &[]), &bp).unwrap();
        assert!(b.argv.iter().any(|x| x == "sandbox_mode=\"read-only\""));
        assert!(
            !b.argv.iter().any(|x| x == "-s"),
            "exec resume has no -s flag"
        );
        assert_eq!(b.argv.last().unwrap(), "-");
    }

    #[test]
    fn opencode_is_always_auto_and_hardened_per_access() {
        let read = build(
            "opencode",
            inp(Access::Read, &[]),
            &BuildPaths {
                last_msg: &paths().0,
                prompt_file: &paths().1,
            },
            None,
        )
        .unwrap();
        assert!(
            read.argv.iter().any(|x| x == "--auto"),
            "ask-permission hangs headless"
        );
        assert!(read
            .argv
            .windows(2)
            .any(|w| w[0] == "--agent" && w[1] == "plan"));
        let cfg = &read.env_set[0].1;
        assert!(
            serde_json::from_str::<serde_json::Value>(cfg).is_ok(),
            "config env must be valid JSON: {cfg}"
        );
        assert!(cfg.contains("\"bash\":\"deny\""));

        let write = build(
            "opencode",
            inp(Access::Write, &[]),
            &BuildPaths {
                last_msg: &paths().0,
                prompt_file: &paths().1,
            },
            None,
        )
        .unwrap();
        assert!(write
            .argv
            .windows(2)
            .any(|w| w[0] == "--agent" && w[1] == "build"));
        assert!(write.env_set[0].1.contains("external_directory"));
        // --auto approves whatever is not denied, and opencode has no sandbox:
        // an auto-approved shell would make write == full.
        assert!(
            write.env_set[0].1.contains("\"bash\":\"deny\""),
            "opencode write must deny bash: {}",
            write.env_set[0].1
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(&write.env_set[0].1).is_ok(),
            "config env must be valid JSON"
        );

        let full = build(
            "opencode",
            inp(Access::Full, &[]),
            &BuildPaths {
                last_msg: &paths().0,
                prompt_file: &paths().1,
            },
            None,
        )
        .unwrap();
        assert!(full.env_set.is_empty(), "full imposes no extra denies");
    }

    #[test]
    fn claude_scrubs_nested_session_env_and_uses_json() {
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        let b = build("claude", inp(Access::Read, &[]), &bp, Some("uuid-1")).unwrap();
        assert!(b
            .argv
            .windows(2)
            .any(|w| w[0] == "--output-format" && w[1] == "json"));
        assert!(b
            .argv
            .windows(2)
            .any(|w| w[0] == "--session-id" && w[1] == "uuid-1"));
        assert_eq!(b.preassigned_session.as_deref(), Some("uuid-1"));
        assert!(b.env_remove.iter().any(|v| v == "CLAUDE_CODE_SESSION_ID"));
        assert!(b.env_remove.iter().any(|v| v == "ANTHROPIC_MODEL"));
    }

    #[test]
    fn agy_effort_is_dropped_when_the_model_id_encodes_it() {
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        let mut i = inp(Access::Read, &[]);
        i.model = Some("gemini-3.1-pro-high".into());
        i.effort = Some("high".into());
        let b = build("agy", i, &bp, None).unwrap();
        assert!(!b.argv.iter().any(|x| x == "--effort"));
        assert!(b.warnings.iter().any(|w| w.contains("already encodes")));
    }

    #[test]
    fn bin_override_replaces_argv0_only() {
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        let mut i = inp(Access::Read, &[]);
        i.bin = Some("/opt/codex.exe".into());
        let b = build("codex", i, &bp, None).unwrap();
        assert_eq!(b.argv[0], "/opt/codex.exe");
        assert_eq!(b.argv[1], "exec");
    }

    #[test]
    fn cursor_pins_trust_and_disables_config_escalation() {
        let read = build_argv("cursor", Access::Read);
        assert!(read
            .windows(2)
            .any(|w| w[0] == "--output-format" && w[1] == "json"));
        assert!(
            read.iter().any(|x| x == "--trust"),
            "headless refuses untrusted dirs"
        );
        // A repo-supplied .cursor/cli.json can set approvalMode "unrestricted".
        assert!(read.iter().any(|x| x == "--disable-project-configs"));
        assert!(read.iter().any(|x| x == "--disable-auto-update"));
        assert!(read.windows(2).any(|w| w[0] == "--mode" && w[1] == "plan"));
        assert!(!read.iter().any(|x| x == "--force"));
        // The prompt must never become a positional: cursor reads stdin only
        // when the positional prompt is empty.
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        assert_eq!(
            build("cursor", inp(Access::Read, &[]), &bp, None)
                .unwrap()
                .delivery,
            Delivery::Stdin
        );

        // cursor headless has no middle tier: full means --force, and write is
        // refused outright instead of silently meaning full.
        assert!(build_argv("cursor", Access::Full)
            .iter()
            .any(|x| x == "--force"));
        for argv in [
            build("cursor", inp(Access::Write, &[]), &bp, None),
            build_resume("cursor", "chat-1", inp(Access::Write, &[]), &bp),
        ] {
            let e = argv.err().expect("cursor write must be rejected");
            assert!(e.contains("two permission tiers"), "{e}");
        }
    }

    #[test]
    fn cursor_resume_reuses_the_resume_flag_and_expects_its_id_back() {
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        let b = build_resume("cursor", "chat-1", inp(Access::Read, &[]), &bp).unwrap();
        assert!(b
            .argv
            .windows(2)
            .any(|w| w[0] == "--resume" && w[1] == "chat-1"));
        assert_eq!(b.expect_session.as_deref(), Some("chat-1"));
        assert_eq!(b.delivery, Delivery::Stdin);
        // A miss silently creates a chat, so sfh pins the directory too.
        assert!(resume_requires_same_cwd("cursor"));
        assert!(!resume_requires_same_cwd("claude"));
        assert!(!supports_fork("cursor"));
    }

    #[test]
    fn fork_builds_a_child_session_for_every_supporting_tool() {
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        for t in ["claude", "opencode", "grok", "pi"] {
            assert!(supports_fork(t), "{t}");
            let b = build_fork(t, "PARENT", "CHILD", inp(Access::Read, &[]), &bp).unwrap();
            assert!(
                b.argv.iter().any(|x| x == "PARENT"),
                "{t}: parent id missing"
            );
            match t {
                // opencode mints the child id itself; it cannot be named.
                "opencode" => {
                    assert!(b.preassigned_session.is_none());
                    assert!(b.argv.iter().any(|x| x == "--fork"));
                }
                _ => {
                    assert_eq!(b.preassigned_session.as_deref(), Some("CHILD"), "{t}");
                    assert!(b.argv.iter().any(|x| x == "CHILD"), "{t}");
                }
            }
            // A fork must never be asserted to equal the parent session.
            assert!(b.expect_session.is_none(), "{t}");
        }
        for t in ["codex", "agy", "cursor"] {
            assert!(!supports_fork(t), "{t}");
            let e = build_fork(t, "P", "C", inp(Access::Read, &[]), &bp)
                .err()
                .unwrap();
            assert!(e.contains("cannot fork"), "{t}: {e}");
        }
    }

    #[test]
    fn only_claude_warms_up_by_default() {
        assert!(fork_warmup_pays("claude"));
        for t in ["opencode", "grok", "pi"] {
            assert!(!fork_warmup_pays(t), "{t} showed no measured saving");
        }
    }

    // claude has no OS sandbox, so an auto-approved Bash at the `write` tier
    // would make write == full while claiming to be a middle ground. This is
    // the same rule pi's write tier follows; the two must not diverge.
    #[test]
    fn claude_write_does_not_auto_approve_a_shell() {
        for argv in [
            build_argv("claude", Access::Write),
            // The resume and fork paths build their own argv; all three have to
            // agree, and they have drifted apart before.
            {
                let l = std::path::PathBuf::from("l");
                let p = std::path::PathBuf::from("p");
                build_resume(
                    "claude",
                    "sess",
                    inp(Access::Write, &[]),
                    &BuildPaths {
                        last_msg: &l,
                        prompt_file: &p,
                    },
                )
                .unwrap()
                .argv
            },
            {
                let l = std::path::PathBuf::from("l");
                let p = std::path::PathBuf::from("p");
                build_fork(
                    "claude",
                    "parent",
                    "child",
                    inp(Access::Write, &[]),
                    &BuildPaths {
                        last_msg: &l,
                        prompt_file: &p,
                    },
                )
                .unwrap()
                .argv
            },
        ] {
            let allowed = argv
                .windows(2)
                .find(|w| w[0] == "--allowedTools")
                .map(|w| w[1].clone())
                .unwrap_or_default();
            assert!(
                !allowed.contains("Bash"),
                "claude write must not auto-approve Bash, got '{allowed}' in {argv:?}"
            );
            assert!(
                argv.windows(2)
                    .any(|w| w[0] == "--permission-mode" && w[1] == "acceptEdits"),
                "claude write should still auto-approve edits: {argv:?}"
            );
        }
        // full still means full - the escape hatch has to keep working.
        assert!(build_argv("claude", Access::Full)
            .iter()
            .any(|x| x == "--dangerously-skip-permissions"));
    }

    #[test]
    fn pi_access_levels_are_a_tool_allowlist() {
        let read = build_argv("pi", Access::Read);
        assert!(read.windows(2).any(|w| w[0] == "--mode" && w[1] == "json"));
        assert!(read
            .windows(2)
            .any(|w| w[0] == "--tools" && w[1] == "read,grep,find,ls"));
        // Project-local extensions run with full process rights, so a read step
        // must not load them.
        for f in [
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-approve",
        ] {
            assert!(read.iter().any(|x| x == f), "{f} missing from pi read");
        }
        assert!(!read.iter().any(|x| x.contains("bash")));

        let write = build_argv("pi", Access::Write);
        assert!(write
            .windows(2)
            .any(|w| w[0] == "--tools" && w[1] == "read,edit,write,grep,find,ls"));
        assert!(
            !write.iter().any(|x| x.contains("bash")),
            "write must not register a shell"
        );
        // Extensions run with full process rights and could register Bash,
        // which would undo the allowlist - write must not load them either.
        for f in [
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-approve",
        ] {
            assert!(write.iter().any(|x| x == f), "{f} missing from pi write");
        }

        let full = build_argv("pi", Access::Full);
        assert!(full
            .windows(2)
            .any(|w| w[0] == "--tools" && w[1] == "read,bash,edit,write,grep,find,ls"));
        assert!(full.iter().any(|x| x == "--approve"));
    }

    #[test]
    fn pi_write_warns_that_it_has_no_shell() {
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        let b = build("pi", inp(Access::Write, &[]), &bp, None).unwrap();
        assert!(
            b.warnings.iter().any(|w| w.contains("no shell tool")),
            "{:?}",
            b.warnings
        );
    }

    #[test]
    fn pi_resume_reuses_session_id_flag_and_reads_stdin() {
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        let b = build_resume("pi", "sess-x", inp(Access::Read, &[]), &bp).unwrap();
        assert_eq!(b.delivery, Delivery::Stdin);
        assert!(b
            .argv
            .windows(2)
            .any(|w| w[0] == "--session-id" && w[1] == "sess-x"));
        // pi has no separate resume verb: create and resume share one flag.
        assert!(!b.argv.iter().any(|x| x == "--resume" || x == "--continue"));
    }

    #[test]
    fn resume_sets_expect_session_for_tools_that_can_silently_start_new() {
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        for t in ["claude", "opencode", "grok", "agy", "pi"] {
            let b = build_resume(t, "sid", inp(Access::Read, &[]), &bp).unwrap();
            assert_eq!(b.expect_session.as_deref(), Some("sid"), "{t}");
        }
    }

    #[test]
    fn access_levels_have_a_strict_ordering() {
        assert!(Access::Read.rank() < Access::Write.rank());
        assert!(Access::Write.rank() < Access::Full.rank());
        for a in [Access::Read, Access::Write, Access::Full] {
            assert_eq!(Access::parse(Some(a.as_str())).unwrap(), a);
        }
    }

    // The audit found seven strings checked by naive contains: pi's -t (which
    // the README itself suggested for adding Bash), claude's --allowedTools,
    // codex's -c sandbox_mode=... and friends all slipped through. Each tool's
    // permission levers must be caught, and harmless lookalikes must not be.
    #[test]
    fn escalation_detection_covers_each_tools_permission_levers() {
        for (tool, arg) in [
            ("pi", "--approve"),
            ("pi", "--tools"),
            ("pi", "-t"),
            ("opencode", "--agent"),
            ("opencode", "--agent=build"),
            ("claude", "--tools"),
            ("claude", "--allowedTools"),
            ("claude", "--permission-mode"),
            ("claude", "--permission-mode=bypassPermissions"),
            ("grok", "--allow"),
            ("grok", "--permission-mode"),
            ("agy", "--mode"),
            ("codex", "-s"),
            ("codex", "--sandbox"),
            ("codex", "sandbox_mode=\"danger-full-access\""),
            ("codex", "approval_policy=\"on-failure\""),
            // tool-independent bypasses
            ("claude", "--force"),
            ("codex", "--yolo"),
            ("grok", "bypassPermissions"),
            ("agy", "--dangerously-skip-permissions"),
        ] {
            assert!(
                is_escalation_arg(tool, arg),
                "{tool}: {arg} slipped through"
            );
        }
        for (tool, arg) in [
            ("codex", "--model"),
            ("codex", "-m"),
            ("codex", "model_reasoning_effort=\"high\""),
            ("claude", "--effort"),
            ("claude", "--output-format"),
            ("opencode", "--variant"),
            ("grok", "--reasoning-effort"),
            ("grok", "--deny"),
            ("agy", "--model"),
            ("agy", "--agent"),
            ("pi", "--thinking"),
            ("pi", "--no-approve"),
            ("pi", "--no-extensions"),
            ("pi", "--session-id"),
            ("cursor", "--mode"),
            ("cursor", "--trust"),
            ("codex", "--force-of-nature"),
            ("agy", "--modest"),
        ] {
            assert!(
                !is_escalation_arg(tool, arg),
                "{tool}: {arg} is a false positive"
            );
        }
    }
}
