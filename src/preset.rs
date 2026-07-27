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

/// Flags a user could put in `args:` that silently escalate past `access:`.
pub const ESCALATION_FLAGS: [&str; 6] = [
    "--dangerously-skip-permissions",
    "--dangerously-bypass-approvals-and-sandbox",
    "bypassPermissions",
    "danger-full-access",
    "--always-approve",
    "--yolo",
];

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
        // Best-effort workspace boundary: --auto would otherwise auto-approve
        // out-of-tree access.
        Access::Write => vec![(
            "OPENCODE_CONFIG_CONTENT".to_string(),
            "{\"permission\":{\"external_directory\":\"deny\"}}".to_string(),
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
            push(&mut a, &["codex", "exec", "--skip-git-repo-check", "--color", "never", "--json"]);
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
            push(&mut a, &["--skip-git-repo-check", "--json", "-c", "approval_policy=\"never\""]);
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
        let bp = BuildPaths { last_msg: &l, prompt_file: &p };
        build(tool, inp(access, &[]), &bp, None).unwrap().argv
    }

    #[test]
    fn agy_prompt_flag_is_last_so_delivery_binds_it() {
        let argv = build_argv("agy", Access::Read);
        assert_eq!(argv.last().unwrap(), "-p", "prompt must attach to -p as its value");
        assert!(argv.windows(2).any(|w| w[0] == "--print-timeout" && w[1] == "900s"));
        assert!(argv.windows(2).any(|w| w[0] == "--mode" && w[1] == "plan"));
    }

    #[test]
    fn grok_uses_prompt_file_never_stdin() {
        let (l, p) = paths();
        let bp = BuildPaths { last_msg: &l, prompt_file: &p };
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
            assert_eq!(argv.last().unwrap(), "-", "codex reads the prompt from stdin");
        }
    }

    #[test]
    fn codex_resume_rebuilds_sandbox_via_config_override() {
        let (l, p) = paths();
        let bp = BuildPaths { last_msg: &l, prompt_file: &p };
        let b = build_resume("codex", "sess-1", inp(Access::Read, &[]), &bp).unwrap();
        assert!(b.argv.iter().any(|x| x == "sandbox_mode=\"read-only\""));
        assert!(!b.argv.iter().any(|x| x == "-s"), "exec resume has no -s flag");
        assert_eq!(b.argv.last().unwrap(), "-");
    }

    #[test]
    fn opencode_is_always_auto_and_hardened_per_access() {
        let read = build("opencode", inp(Access::Read, &[]), &BuildPaths { last_msg: &paths().0, prompt_file: &paths().1 }, None).unwrap();
        assert!(read.argv.iter().any(|x| x == "--auto"), "ask-permission hangs headless");
        assert!(read.argv.windows(2).any(|w| w[0] == "--agent" && w[1] == "plan"));
        let cfg = &read.env_set[0].1;
        assert!(serde_json::from_str::<serde_json::Value>(cfg).is_ok(), "config env must be valid JSON: {cfg}");
        assert!(cfg.contains("\"bash\":\"deny\""));

        let write = build("opencode", inp(Access::Write, &[]), &BuildPaths { last_msg: &paths().0, prompt_file: &paths().1 }, None).unwrap();
        assert!(write.argv.windows(2).any(|w| w[0] == "--agent" && w[1] == "build"));
        assert!(write.env_set[0].1.contains("external_directory"));

        let full = build("opencode", inp(Access::Full, &[]), &BuildPaths { last_msg: &paths().0, prompt_file: &paths().1 }, None).unwrap();
        assert!(full.env_set.is_empty(), "full imposes no extra denies");
    }

    #[test]
    fn claude_scrubs_nested_session_env_and_uses_json() {
        let (l, p) = paths();
        let bp = BuildPaths { last_msg: &l, prompt_file: &p };
        let b = build("claude", inp(Access::Read, &[]), &bp, Some("uuid-1")).unwrap();
        assert!(b.argv.windows(2).any(|w| w[0] == "--output-format" && w[1] == "json"));
        assert!(b.argv.windows(2).any(|w| w[0] == "--session-id" && w[1] == "uuid-1"));
        assert_eq!(b.preassigned_session.as_deref(), Some("uuid-1"));
        assert!(b.env_remove.iter().any(|v| v == "CLAUDE_CODE_SESSION_ID"));
        assert!(b.env_remove.iter().any(|v| v == "ANTHROPIC_MODEL"));
    }

    #[test]
    fn agy_effort_is_dropped_when_the_model_id_encodes_it() {
        let (l, p) = paths();
        let bp = BuildPaths { last_msg: &l, prompt_file: &p };
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
        let bp = BuildPaths { last_msg: &l, prompt_file: &p };
        let mut i = inp(Access::Read, &[]);
        i.bin = Some("/opt/codex.exe".into());
        let b = build("codex", i, &bp, None).unwrap();
        assert_eq!(b.argv[0], "/opt/codex.exe");
        assert_eq!(b.argv[1], "exec");
    }

    #[test]
    fn resume_sets_expect_session_for_tools_that_can_silently_start_new() {
        let (l, p) = paths();
        let bp = BuildPaths { last_msg: &l, prompt_file: &p };
        for t in ["claude", "opencode", "grok", "agy"] {
            let b = build_resume(t, "sid", inp(Access::Read, &[]), &bp).unwrap();
            assert_eq!(b.expect_session.as_deref(), Some("sid"), "{t}");
        }
    }
}
