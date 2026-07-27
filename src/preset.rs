//! Preset command builders for the supported AI CLIs.
//!
//! Grounded in live-verified research (2026-07-27) against:
//!   codex-cli 0.146.0-alpha.3.1, claude 2.1.220, opencode 1.18.3,
//!   grok 0.2.112, agy 1.0.8.
//! Key facts encoded here:
//! - codex: stdin '-' prompt; -o last-message file; `exec resume` has no -s flag,
//!   sandbox must be re-specified via -c sandbox_mode=...; session id parsed from stderr.
//! - claude: stdin prompt; plan mode is a soft guarantee -> read uses dontAsk + --tools
//!   whitelist; --session-id preassign + `-p -r <uuid>` resume; scrub CLAUDE_* env.
//! - opencode: stdin prompt; --auto is REQUIRED headless (permission "ask" hangs forever);
//!   banner goes to stderr (stdout clean); session id on every --format json line.
//! - grok: prompt ONLY via --prompt-file (stdin opens the TUI and hangs); --permission-mode
//!   plan is compat-only -> read uses dontAsk + --deny; --session-id preassign + --resume.
//! - agy: prompt ONLY as the value of -p (Go flag, must be adjacent); hidden
//!   --output-format json exposes conversation_id/status/response; --print-timeout
//!   defaults to 5m and kills long runs; unknown --conversation id silently starts new.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq)]
pub enum Access {
    Read,
    Write,
    Full,
}

impl Access {
    pub fn parse(s: Option<&str>) -> Result<Access, String> {
        match s.unwrap_or("write") {
            "read" => Ok(Access::Read),
            "write" => Ok(Access::Write),
            "full" => Ok(Access::Full),
            other => Err(format!("access must be read/write/full, got '{other}'")),
        }
    }
}

/// How to turn the raw process output into the step's chain output.
pub enum OutputParse {
    Stdout,
    /// codex --output-last-message file; fall back to stdout if missing/empty.
    LastMsgFile(PathBuf),
    /// opencode --format json NDJSON: concat text events of the last message.
    OpencodeNdjson,
    /// agy --output-format json single-line envelope: .response / .status / .conversation_id.
    AgyJson,
}

/// How the rendered prompt reaches the tool.
#[derive(Clone, Copy, PartialEq)]
pub enum Delivery {
    Stdin,
    /// Passed via a file flag (grok --prompt-file); nothing on stdin.
    PromptFile,
    /// Appended as the final argv element, adjacent to a preceding value-taking
    /// flag or as a positional (agy -p <prompt>). Size-capped by the caller.
    Arg,
    None,
}

/// How to obtain the session id after a run (for continue_from).
#[derive(Clone, PartialEq)]
pub enum SessionCapture {
    /// We passed the id ourselves (claude --session-id, grok --session-id, any resume).
    Preassigned(String),
    /// codex: parse "session id: <uuid>" from the stderr transcript.
    CodexStderr,
    /// opencode --format json: top-level sessionID on every NDJSON line.
    OpencodeNdjson,
    /// agy --output-format json: top-level conversation_id.
    AgyJson,
    Unsupported,
}

pub struct Built {
    pub argv: Vec<String>,
    pub parse: OutputParse,
    pub delivery: Delivery,
    pub session: SessionCapture,
    /// Env vars to remove from the child (claude: nested-session vars).
    pub env_remove: Vec<String>,
    /// Env vars to set on the child (opencode: read-hardening config).
    pub env_set: Vec<(String, String)>,
    /// When resuming: the session id the tool MUST echo back (agy silently
    /// starts a new conversation on an unknown id - detect that as failure).
    pub expect_session: Option<String>,
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

/// True when this tool needs a pre-assigned session id to make the run resumable.
pub fn wants_preassign(tool: &str) -> bool {
    matches!(tool, "claude" | "grok")
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
/// claude write-level: acceptEdits auto-approves fs edits; shell & web need explicit allows
/// or a -p run aborts on first denial.
const CLAUDE_WRITE_ALLOWED: &str = "Bash,WebSearch,WebFetch";

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

fn finish(mut b: Built, bin: Option<String>) -> Built {
    if let Some(bin) = bin {
        b.argv[0] = bin;
    }
    b
}

/// Build the command line for a fresh (non-resumed) preset run.
/// `preassign_session`: uuid to preassign when the tool supports it.
/// `want_capture`: this step is a continue_from target, so the run must yield a session id.
pub fn build(
    tool: &str,
    inp: PresetInput,
    paths: &BuildPaths,
    preassign_session: Option<&str>,
    want_capture: bool,
) -> Result<Built, String> {
    let mut a: Vec<String> = Vec::new();
    let mut warnings = Vec::new();
    let mut env_remove = Vec::new();
    let mut env_set = Vec::new();
    let parse;
    let delivery;
    let mut session = SessionCapture::Unsupported;
    match tool {
        "codex" => {
            push(&mut a, &["codex", "exec", "--skip-git-repo-check", "--color", "never"]);
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
            parse = OutputParse::LastMsgFile(paths.last_msg.to_path_buf());
            delivery = Delivery::Stdin;
            session = SessionCapture::CodexStderr;
        }
        "claude" => {
            push(&mut a, &["claude", "-p", "--output-format", "text"]);
            if let Some(m) = &inp.model {
                push(&mut a, &["--model"]);
                a.push(m.clone());
            }
            if let Some(e) = &inp.effort {
                push(&mut a, &["--effort"]);
                a.push(e.clone());
            }
            if let Some(ag) = &inp.agent {
                push(&mut a, &["--agent"]);
                a.push(ag.clone());
            }
            match inp.access {
                Access::Read => {
                    push(&mut a, &["--permission-mode", "dontAsk", "--tools", CLAUDE_READ_TOOLS])
                }
                Access::Write => push(
                    &mut a,
                    &["--permission-mode", "acceptEdits", "--allowedTools", CLAUDE_WRITE_ALLOWED],
                ),
                Access::Full => push(&mut a, &["--dangerously-skip-permissions"]),
            }
            if let Some(id) = preassign_session {
                push(&mut a, &["--session-id"]);
                a.push(id.to_string());
                session = SessionCapture::Preassigned(id.to_string());
            }
            a.extend(inp.extra.iter().cloned());
            env_remove = CLAUDE_ENV_SCRUB.iter().map(|s| s.to_string()).collect();
            parse = OutputParse::Stdout;
            delivery = Delivery::Stdin;
        }
        "opencode" => {
            push(&mut a, &["opencode", "run"]);
            if let Some(m) = &inp.model {
                push(&mut a, &["-m"]);
                a.push(m.clone());
            }
            if let Some(e) = &inp.effort {
                push(&mut a, &["--variant"]);
                a.push(e.clone());
            }
            let agent_name = match (&inp.agent, inp.access) {
                (Some(ag), _) => {
                    push(&mut a, &["--agent"]);
                    a.push(ag.clone());
                    ag.clone()
                }
                (None, Access::Read) => {
                    push(&mut a, &["--agent", "plan"]);
                    "plan".to_string()
                }
                (None, _) => {
                    // Pin the agent so write/full don't inherit whatever default
                    // agent the user's opencode config names.
                    push(&mut a, &["--agent", "build"]);
                    "build".to_string()
                }
            };
            // --auto is mandatory headless: any permission that resolves to "ask"
            // has no UI in `opencode run` and hangs forever. Explicit deny rules
            // are still enforced with --auto.
            push(&mut a, &["--auto"]);
            match inp.access {
                Access::Read => {
                    // The stock plan agent denies edits but NOT bash (1.18.3), so a read
                    // step could still write via shell redirection. Merge a per-run deny
                    // through OPENCODE_CONFIG_CONTENT (highest-precedence config layer).
                    env_set.push((
                        "OPENCODE_CONFIG_CONTENT".to_string(),
                        format!(
                            "{{\"agent\":{{\"{agent_name}\":{{\"permission\":{{\"edit\":\"deny\",\"bash\":\"deny\"}}}}}}}}"
                        ),
                    ));
                }
                Access::Write => {
                    // Best-effort workspace boundary: --auto would otherwise
                    // auto-approve out-of-tree access. full omits this.
                    env_set.push((
                        "OPENCODE_CONFIG_CONTENT".to_string(),
                        "{\"permission\":{\"external_directory\":\"deny\"}}".to_string(),
                    ));
                }
                Access::Full => {}
            }
            a.extend(inp.extra.iter().cloned());
            if want_capture {
                push(&mut a, &["--format", "json"]);
                parse = OutputParse::OpencodeNdjson;
                session = SessionCapture::OpencodeNdjson;
            } else {
                parse = OutputParse::Stdout;
            }
            delivery = Delivery::Stdin;
        }
        "grok" => {
            push(&mut a, &["grok", "--output-format", "plain"]);
            if let Some(m) = &inp.model {
                push(&mut a, &["-m"]);
                a.push(m.clone());
            }
            if let Some(e) = &inp.effort {
                push(&mut a, &["--reasoning-effort"]);
                a.push(e.clone());
            }
            if let Some(ag) = &inp.agent {
                push(&mut a, &["--agent"]);
                a.push(ag.clone());
            }
            // --permission-mode plan is compat-only in headless and --sandbox is a
            // no-op on Windows, so read = dontAsk + hard deny rules (deny always wins).
            match inp.access {
                Access::Read => push(
                    &mut a,
                    &["--permission-mode", "dontAsk", "--deny", "Edit", "--deny", "Write", "--deny", "Bash"],
                ),
                Access::Write => {
                    push(&mut a, &["--permission-mode", "acceptEdits"]);
                    warnings.push(
                        "grok write auto-approves edits only; shell commands are not auto-approved (add args: [\"--allow\", \"Bash(...)\"] if the step must run commands)".into(),
                    );
                }
                Access::Full => push(&mut a, &["--permission-mode", "bypassPermissions"]),
            }
            if let Some(id) = preassign_session {
                push(&mut a, &["--session-id"]);
                a.push(id.to_string());
                session = SessionCapture::Preassigned(id.to_string());
            }
            a.extend(inp.extra.iter().cloned());
            push(&mut a, &["--prompt-file"]);
            a.push(paths.prompt_file.display().to_string());
            parse = OutputParse::Stdout;
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
            // Always JSON: the envelope's status field corrects agy's unreliable
            // exit codes (valid completions can exit 1, errors can exit 0).
            push(&mut a, &["--output-format", "json"]);
            parse = OutputParse::AgyJson;
            if want_capture {
                session = SessionCapture::AgyJson;
            }
            // -p is a Go string flag: the prompt MUST be the next argv element.
            // Keep this last; Delivery::Arg appends the prompt right after it.
            push(&mut a, &["-p"]);
            delivery = Delivery::Arg;
        }
        other => {
            return Err(format!(
                "unknown tool '{other}' (use {}, or a custom cmd:)",
                crate::flow::TOOLS.join("/")
            ))
        }
    }
    Ok(finish(
        Built {
            argv: a,
            parse,
            delivery,
            session,
            env_remove,
            env_set,
            expect_session: None,
            warnings,
        },
        inp.bin,
    ))
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
            push(&mut a, &["--skip-git-repo-check", "-c", "approval_policy=\"never\""]);
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
            parse = OutputParse::LastMsgFile(paths.last_msg.to_path_buf());
            delivery = Delivery::Stdin;
        }
        "claude" => {
            push(&mut a, &["claude", "-p", "--output-format", "text", "-r"]);
            a.push(session_id.to_string());
            if let Some(m) = &inp.model {
                push(&mut a, &["--model"]);
                a.push(m.clone());
            }
            if let Some(e) = &inp.effort {
                push(&mut a, &["--effort"]);
                a.push(e.clone());
            }
            if let Some(ag) = &inp.agent {
                push(&mut a, &["--agent"]);
                a.push(ag.clone());
            }
            match inp.access {
                Access::Read => {
                    push(&mut a, &["--permission-mode", "dontAsk", "--tools", CLAUDE_READ_TOOLS])
                }
                Access::Write => push(
                    &mut a,
                    &["--permission-mode", "acceptEdits", "--allowedTools", CLAUDE_WRITE_ALLOWED],
                ),
                Access::Full => push(&mut a, &["--dangerously-skip-permissions"]),
            }
            a.extend(inp.extra.iter().cloned());
            env_remove = CLAUDE_ENV_SCRUB.iter().map(|s| s.to_string()).collect();
            parse = OutputParse::Stdout;
            delivery = Delivery::Stdin;
        }
        "opencode" => {
            push(&mut a, &["opencode", "run", "-s"]);
            a.push(session_id.to_string());
            if let Some(m) = &inp.model {
                push(&mut a, &["-m"]);
                a.push(m.clone());
            }
            if let Some(e) = &inp.effort {
                push(&mut a, &["--variant"]);
                a.push(e.clone());
            }
            let agent_name = match (&inp.agent, inp.access) {
                (Some(ag), _) => {
                    push(&mut a, &["--agent"]);
                    a.push(ag.clone());
                    ag.clone()
                }
                (None, Access::Read) => {
                    push(&mut a, &["--agent", "plan"]);
                    "plan".to_string()
                }
                (None, _) => {
                    push(&mut a, &["--agent", "build"]);
                    "build".to_string()
                }
            };
            push(&mut a, &["--auto"]);
            match inp.access {
                Access::Read => {
                    env_set.push((
                        "OPENCODE_CONFIG_CONTENT".to_string(),
                        format!(
                            "{{\"agent\":{{\"{agent_name}\":{{\"permission\":{{\"edit\":\"deny\",\"bash\":\"deny\"}}}}}}}}"
                        ),
                    ));
                }
                Access::Write => {
                    env_set.push((
                        "OPENCODE_CONFIG_CONTENT".to_string(),
                        "{\"permission\":{\"external_directory\":\"deny\"}}".to_string(),
                    ));
                }
                Access::Full => {}
            }
            a.extend(inp.extra.iter().cloned());
            parse = OutputParse::Stdout;
            delivery = Delivery::Stdin;
        }
        "grok" => {
            push(&mut a, &["grok", "--output-format", "plain", "--resume"]);
            a.push(session_id.to_string());
            if let Some(m) = &inp.model {
                push(&mut a, &["-m"]);
                a.push(m.clone());
            }
            if let Some(e) = &inp.effort {
                push(&mut a, &["--reasoning-effort"]);
                a.push(e.clone());
            }
            if let Some(ag) = &inp.agent {
                push(&mut a, &["--agent"]);
                a.push(ag.clone());
            }
            match inp.access {
                Access::Read => push(
                    &mut a,
                    &["--permission-mode", "dontAsk", "--deny", "Edit", "--deny", "Write", "--deny", "Bash"],
                ),
                Access::Write => {
                    push(&mut a, &["--permission-mode", "acceptEdits"]);
                    warnings.push(
                        "grok write auto-approves edits only; shell commands are not auto-approved (add args: [\"--allow\", \"Bash(...)\"] if the step must run commands)".into(),
                    );
                }
                Access::Full => push(&mut a, &["--permission-mode", "bypassPermissions"]),
            }
            a.extend(inp.extra.iter().cloned());
            push(&mut a, &["--prompt-file"]);
            a.push(paths.prompt_file.display().to_string());
            parse = OutputParse::Stdout;
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
            // Resume MUST use json output: agy silently starts a NEW conversation on an
            // unknown id, and only the JSON envelope lets us detect that (expect_session).
            push(&mut a, &["--output-format", "json"]);
            expect_session = Some(session_id.to_string());
            push(&mut a, &["-p"]);
            parse = OutputParse::AgyJson;
            delivery = Delivery::Arg;
        }
        other => return Err(format!("tool '{other}' does not support continue_from")),
    }
    Ok(finish(
        Built {
            argv: a,
            parse,
            delivery,
            session: SessionCapture::Preassigned(session_id.to_string()),
            env_remove,
            env_set,
            expect_session,
            warnings,
        },
        inp.bin,
    ))
}
