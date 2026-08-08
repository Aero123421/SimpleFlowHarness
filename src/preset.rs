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
//!   `--json` stdout carries thread.started/turn.completed; `exec resume` and
//!   `exec fork` (P1-07) both lack -s, so the sandbox is re-specified with
//!   -c sandbox_mode=...; fork ships after this file's LAST_VERIFIED date, so
//!   sfh additionally demands a live --help probe of the installed binary
//!   before it will use it (see `codex_fork_confirmed`).
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
    pub(crate) invalid_cost: bool,
}

impl Usage {
    fn normalize_cost(cost: f64) -> f64 {
        if cost.is_nan() || cost < 0.0 || cost == f64::NEG_INFINITY {
            0.0
        } else if cost == f64::INFINITY {
            f64::MAX
        } else {
            cost
        }
    }

    /// Cost reported by an external tool is untrusted accounting input.
    /// Negative/NaN values cannot refund earlier work, while positive infinity
    /// is treated as the largest representable spend so it fails a finite
    /// budget closed instead of silently becoming free.
    pub fn reported_cost(&self) -> f64 {
        match self.cost_usd {
            Some(c) => Self::normalize_cost(c),
            None => 0.0,
        }
    }

    /// Add one provider report while preserving the fact that any component
    /// was invalid. Normalizing only after summing would let a negative entry
    /// refund a positive entry within one streamed response.
    pub fn add_reported_cost(&mut self, cost: f64) {
        let normalized = Self::normalize_cost(cost);
        self.invalid_cost |= cost.to_bits() != normalized.to_bits();
        let sum = self.reported_cost() + normalized;
        self.cost_usd = Some(if sum.is_finite() { sum } else { f64::MAX });
    }

    /// Normalize an external report before it is printed, logged, or returned
    /// to the engine. Returns true when the provider supplied an invalid value.
    pub fn sanitize_reported(&mut self) -> bool {
        let Some(original) = self.cost_usd else {
            return false;
        };
        let normalized = self.reported_cost();
        let changed = self.invalid_cost || original.to_bits() != normalized.to_bits();
        if changed {
            self.cost_usd = Some(normalized);
        }
        self.invalid_cost = false;
        changed
    }

    /// Add another attempt without losing either attempt's token or cost
    /// accounting. Integer totals saturate, and a floating-point overflow
    /// saturates to f64::MAX so the budget guard remains effective.
    pub fn accumulate(&mut self, other: &Usage) {
        fn add_tokens(total: &mut Option<u64>, value: Option<u64>) {
            if let Some(value) = value {
                *total = Some(total.unwrap_or(0).saturating_add(value));
            }
        }

        add_tokens(&mut self.input_tokens, other.input_tokens);
        add_tokens(&mut self.output_tokens, other.output_tokens);
        self.invalid_cost |= other.invalid_cost
            || other
                .cost_usd
                .is_some_and(|cost| cost.to_bits() != Self::normalize_cost(cost).to_bits());
        if self.cost_usd.is_some() || other.cost_usd.is_some() {
            let sum = self.reported_cost() + other.reported_cost();
            self.cost_usd = Some(if sum.is_finite() { sum } else { f64::MAX });
        }
    }
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
/// project-locally then globally, and opencode session ids are global. codex
/// stays out of this list for the same reason it is absent from
/// `session_is_cwd_scoped`: sfh has no evidence that codex scopes a thread id
/// to the directory it was created in, so treating a cwd change as risky here
/// would warn about a danger nothing has shown to exist.
pub fn fork_is_cwd_scoped(tool: &str) -> bool {
    matches!(tool, "claude" | "grok")
}

/// Tools that can branch a session headlessly into a NEW independent session.
/// codex's `exec fork` (P1-07) joined this list after this file's
/// LAST_VERIFIED baseline was pinned, so `true` here is only the adapter-wide
/// half of the answer: `build_fork`'s codex arm additionally demands live
/// proof from the installed binary before it actually emits anything (see
/// `codex_fork_confirmed`) - this function alone is not permission to launch
/// it. agy has no fork at all.
pub fn supports_fork(tool: &str) -> bool {
    matches!(tool, "claude" | "opencode" | "grok" | "pi" | "codex")
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
/// turned $0.0337 per child into $0.0026. Only claude showed a real saving;
/// codex fork (P1-07) has never been run through this measurement and is left
/// out rather than assumed to share claude's cache economics.
pub fn fork_warmup_pays(tool: &str) -> bool {
    tool == "claude"
}

/// How much of a run's spend the adapter can actually account for.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Coverage {
    /// The tool reports a cost in its own currency terms.
    Cost,
    /// Token counts only; a cost ceiling cannot be enforced from them.
    TokensOnly,
}

impl Coverage {
    pub fn as_str(self) -> &'static str {
        match self {
            Coverage::Cost => "cost",
            Coverage::TokensOnly => "tokens-only",
        }
    }
}

/// How far `access:` can actually be enforced for a tool, per level. sfh's
/// `access:` is a request to the CLI's own permission system, never an OS
/// sandbox, and the four answers below are the honest range of what a CLI does
/// with that request.
///
/// The bar for `Enforced` (P0-02, after the review found it over-claimed for
/// claude/opencode/grok/pi/cursor): the CLI itself must guarantee that ONE
/// flag or mode sfh passes closes the WHOLE class of access the level names.
/// "sfh enumerated the builtin tools it knew about and denied them" is not
/// that guarantee - a builtin-tool allowlist says nothing about an MCP tool,
/// a plugin, a hook, a subagent, or a project instruction file, all of which
/// reach the same capabilities through a door the allowlist never named. The
/// preset author's list can be complete on the day it is written and wrong a
/// release later, because the surface it did not enumerate is precisely the
/// part nobody was looking at. A holistic guarantee looks different: codex's
/// `-s` picks an OS sandbox tier that bounds the whole process, not just the
/// tools codex itself shipped with. When in doubt between `Enforced` and
/// `BestEffort`, the honest default is `BestEffort` - it costs a warning in
/// `preflight`, where over-claiming costs an operator a false sense of a
/// boundary that was never really there.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Enforcement {
    /// The tool has a real sandbox for this level.
    Sandboxed,
    /// The CLI's own flag or mode is documented to close the entire class of
    /// access this level names - not just the specific tools, edits or
    /// commands sfh's preset happens to enumerate. See the enum doc comment
    /// for the bar this has to clear.
    ///
    /// No adapter clears this bar today: agy's P0-02 pass (the last of the
    /// seven) found its `--mode` is the same bare, non-sandboxed switch as
    /// every other downgraded adapter's, leaving `Sandboxed` (codex's real OS
    /// sandbox) the only mechanism currently on the holistic side of the
    /// line. The variant stays - removing it would silently drop `"enforced"`
    /// from the `access_enforcement` vocabulary CHANGELOG v1.4.0 already
    /// documented as machine-readable output, and a tool that closes a whole
    /// access class through its own guarantee without an OS sandbox is a real
    /// case this taxonomy still needs to be able to say.
    #[allow(dead_code)]
    Enforced,
    /// Requested, but the tool's own defaults or config can widen it - or the
    /// preset only closes the surface its author enumerated (builtin tools),
    /// while MCP tools, plugins, hooks, subagents, skills, instruction files
    /// or auto-update sit outside it, unverified. `known_gaps` names which of
    /// these actually apply to this adapter.
    BestEffort,
    /// The tool has no such level; sfh refuses the combination.
    Unsupported,
}

impl Enforcement {
    pub fn as_str(self) -> &'static str {
        match self {
            Enforcement::Sandboxed => "sandboxed",
            Enforcement::Enforced => "enforced",
            Enforcement::BestEffort => "best-effort",
            Enforcement::Unsupported => "unsupported",
        }
    }
}

/// What sfh knows about one adapter without running a model.
///
/// `minimum_version` stays `None` unless a floor is independently documented
/// somewhere sfh can point to - a CLI's own changelog or release notes saying
/// a feature this adapter depends on shipped in version Y, not a number
/// recalled from memory or inferred from LAST_VERIFIED. Pinning a number sfh
/// has not seen documented would let `preflight` report a confident-looking
/// floor it never verified, which is the exact failure this field exists to
/// avoid (P1-06). A live probe against the running binary is a DIFFERENT,
/// stronger check that `sfh doctor` performs; `minimum_version` only claims
/// what the CLI's own authors put in writing. Every `Some` must therefore
/// carry a comment at its `adapter_info` match arm citing that source, so the
/// next reader can tell a documented floor from a guess without redoing the
/// research. Where it stays `None`, `preflight` prints the installed version
/// and says the required floor is unknown, which is a true statement a user
/// can act on.
#[derive(Clone, Debug)]
pub struct AdapterInfo {
    pub tool: &'static str,
    pub default_program: String,
    /// When this adapter's command line was last checked against the real CLI.
    pub last_verified: &'static str,
    /// `None` unless a floor is documented (see the struct doc comment); every
    /// `Some` cites its source where it is pinned in `adapter_info`.
    pub minimum_version: Option<&'static str>,
    /// The structured protocol its output must complete.
    pub protocol: &'static str,
    pub supports_resume: bool,
    pub supports_fork: bool,
    pub cost_coverage: Coverage,
    /// Enforcement for read / write / full, in that order.
    pub policy_coverage: [Enforcement; 3],
    /// Flags this adapter's command line depends on. `preflight` looks for
    /// these in the CLI's own `--help` so a renamed or removed flag surfaces
    /// before a paid run instead of halfway through one.
    pub required_flags: &'static [&'static str],
    /// Known, documented gaps between what `access:` asks for and what the tool
    /// will actually hold to.
    pub known_gaps: &'static [&'static str],
    /// Whether this CLI's exit status can be believed over its own protocol.
    /// False only where the tool's documentation says the exit code is
    /// unreliable; it decides the default for `exit_conflict:`.
    pub exit_code_trustworthy: bool,
}

impl AdapterInfo {
    pub fn enforcement(&self, access: Access) -> Enforcement {
        self.policy_coverage[match access {
            Access::Read => 0,
            Access::Write => 1,
            Access::Full => 2,
        }]
    }
}

/// The date the header of this module records as its live-verification point.
pub const LAST_VERIFIED: &str = "2026-07-27";

/// The per-tool literals `adapter_info` switches on, in the order they fill
/// `AdapterInfo`: protocol, cost coverage, read/write/full enforcement,
/// required flags, known gaps, and a documented minimum version (or `None`).
/// Named so the match below reads as data, not a six-tuple clippy has to
/// squint at.
type AdapterFacts = (
    &'static str,
    Coverage,
    [Enforcement; 3],
    &'static [&'static str],
    &'static [&'static str],
    Option<&'static str>,
);

/// Metadata for one preset, or `None` for a name that is not a preset.
pub fn adapter_info(tool: &str) -> Option<AdapterInfo> {
    use Enforcement::*;
    let (protocol, cost, policy, flags, gaps, min_version): AdapterFacts = match tool {
        "codex" => (
            "codex-jsonl",
            Coverage::TokensOnly,
            // codex is the one preset with a real OS sandbox behind -s: the
            // sandbox bounds the whole process, not just the tools codex
            // itself ships with, so it clears the P0-02 bar for `Enforced`.
            [Sandboxed, Sandboxed, BestEffort],
            &[
                "exec",
                "--skip-git-repo-check",
                "--color",
                "--json",
                "-c",
                "-s",
                "--output-last-message",
                "--dangerously-bypass-approvals-and-sandbox",
            ],
            &["access: full disables the sandbox entirely (--dangerously-bypass-approvals-and-sandbox)"],
            None,
        ),
        "claude" => (
            "claude-json",
            Coverage::Cost,
            // P0-02: downgraded from Enforced. --tools/--allowedTools only
            // close the builtin tools sfh enumerated; see known_gaps for what
            // that enumeration does not reach.
            [BestEffort, BestEffort, BestEffort],
            &[
                "-p",
                "--output-format",
                "--model",
                "--effort",
                "--agent",
                "--permission-mode",
                "--tools",
                "--allowedTools",
                "--dangerously-skip-permissions",
                "--session-id",
                "--fork-session",
            ],
            &[
                "plan mode is advisory, so read is enforced by an explicit --tools allowlist rather than by a sandbox",
                "MCP tools live in a permission namespace separate from --tools/--allowedTools, so an MCP server the project wires up is not covered by either allowlist",
                "plugins, hooks and skills run project- or user-authored code outside the --tools surface entirely, and no flag sfh passes disables them",
                "global (user-level) and project-level instruction files (e.g. CLAUDE.md) load into context unfiltered; sfh neither inspects nor suppresses them",
            ],
            None,
        ),
        "opencode" => (
            "opencode-ndjson",
            Coverage::Cost,
            // P0-02: downgraded from Enforced. OPENCODE_CONFIG_CONTENT only
            // denies the permission keys sfh names; --auto approves whatever
            // it does not name, so an unnamed capability defaults to open.
            [BestEffort, BestEffort, BestEffort],
            &["run", "--format", "--variant", "--agent", "--auto", "--fork"],
            &[
                "read/write are enforced through OPENCODE_CONFIG_CONTENT, which merges with the user's own config",
                "there is no OS sandbox, so write denies bash outright",
                "--auto approves anything the config does not explicitly deny, so a capability this preset's deny list omits defaults to allowed",
                "task (subagent) and skill invocations are not covered by the edit/bash/external_directory denies this preset writes",
                "MCP servers and custom tools sit outside the permission keys this preset sets, and plugins run outside the permission system entirely",
            ],
            None,
        ),
        "grok" => (
            "grok-json",
            Coverage::Cost,
            // P0-02: downgraded from Enforced. --deny only names Edit/Write/
            // Bash; MCPTool is documented as its own permission, and sandbox
            // vs. permission are separate axes grok never promised --deny covers.
            [BestEffort, BestEffort, BestEffort],
            &[
                "--output-format",
                "--reasoning-effort",
                "--agent",
                "--permission-mode",
                "--deny",
                "--session-id",
                "--prompt-file",
                "--resume",
                "--fork-session",
            ],
            &[
                "no OS sandbox; read is a permission-mode plus explicit --deny rules, and grok documents sandbox and permission as separate axes",
                "MCPTool is a permission distinct from Edit/Write/Bash, so an MCP-provided tool is not covered by --deny Edit/Write/Bash",
                "plugins, hooks, skills and subagents are undocumented for headless denial, so sfh cannot say whether --deny reaches them",
                "no flag sfh has confirmed for the pinned grok CLI disables auto-update, so a scripted run's binary could change mid-flow",
            ],
            None,
        ),
        "agy" => (
            "agy-json",
            Coverage::TokensOnly,
            // P0-02: downgraded from Enforced. agy's builder pushes nothing
            // for read/write but the bare --mode flag itself - no --tools-
            // style allowlist, no sandbox flag - identical in shape to
            // cursor's --mode plan (BestEffort) and to claude's plan mode,
            // whose own gap text calls plan mode advisory. A mode switch is a
            // request to agy's own permission system, not a demonstrated
            // guarantee that it bounds the whole process, so nothing here
            // clears the bar Enforced requires.
            [BestEffort, BestEffort, BestEffort],
            &[
                "--model",
                "--effort",
                "--agent",
                "--print-timeout",
                "--mode",
                "--dangerously-skip-permissions",
                "--output-format",
                "-p",
                "--conversation",
            ],
            &[
                "exit codes are unreliable; sfh trusts the envelope's status field",
                "no fork: a branch of an existing conversation is not available headlessly",
                "--mode plan/accept-edits is a bare mode switch - no --tools-style allowlist and no sandbox flag back it, so its reach over an MCP server, a plugin, a hook or a subagent agy loads is undocumented",
                "project- or user-level instruction files agy may read are not inspected or suppressed by any flag this preset passes",
            ],
            // P1-06: agy's own changelog documents structured print output
            // (the --output-format json envelope this preset's whole parse
            // path depends on) as shipping in 1.1.8. LAST_VERIFIED here is
            // 1.0.8 - a version below the floor the feature needs - so a
            // build between those two numbers would accept this preset's
            // flags and then have no structured envelope to answer with.
            // Pinning the floor is a claim about what agy's authors put in
            // writing, not a re-verification of the live-verified research
            // date above; it does not move LAST_VERIFIED.
            Some("1.1.8"),
        ),
        "pi" => (
            "pi-jsonl",
            Coverage::Cost,
            // P0-02: downgraded from Enforced. The --tools allowlist closes
            // pi's own tool surface, but AGENTS.md/CLAUDE.md and a SYSTEM or
            // APPEND_SYSTEM environment variable reach the model outside it.
            [BestEffort, BestEffort, BestEffort],
            &[
                "--mode",
                "--offline",
                "--model",
                "--thinking",
                "--tools",
                "--no-extensions",
                "--no-skills",
                "--no-prompt-templates",
                "--no-context-files",
                "--no-approve",
                "--approve",
                "--session-id",
                "--fork",
            ],
            &[
                "no sandbox at all: access is expressed purely as a --tools allowlist, and write therefore excludes bash",
                "--session-id CREATES a session when the id is not found in this cwd, so a resume is only trustworthy with the session marker",
                "--no-context-files stops pi reading AGENTS.md/CLAUDE.md off disk, but a SYSTEM or APPEND_SYSTEM environment variable injects a hidden system prompt through a path sfh does not scrub",
            ],
            None,
        ),
        "cursor" => (
            "cursor-json",
            Coverage::TokensOnly,
            // Headless cursor has exactly two tiers; there is no write.
            // P0-02: read downgraded from Enforced. Public docs describe print
            // mode as reaching every tool; --mode=plan is a per-action gate on
            // top of that, not a narrower tool list, and its reach over project/
            // global rules, MCP and any headless sandbox is unverified.
            [BestEffort, Unsupported, BestEffort],
            &[
                "-p",
                "--output-format",
                "--trust",
                "--disable-auto-update",
                "--disable-project-configs",
                "--model",
                "--mode",
                "--force",
                "--resume",
            ],
            &[
                "headless permissions are binary: deny-all without --force, approve-all with it, so access: write is refused rather than silently promoted",
                "--resume creates a chat when the id is unknown, so sfh verifies the chat store on disk",
                "print mode exposes the full tool surface regardless of --mode; plan mode denies gated operations rather than narrowing which tools exist, and whether project/global Cursor rules still apply under it is undocumented",
                "MCP servers configured for the project are a separate surface from the built-in tools --mode=plan is documented against",
            ],
            None,
        ),
        _ => return None,
    };
    Some(AdapterInfo {
        tool: crate::flow::TOOLS.iter().find(|t| **t == tool)?,
        default_program: default_program(tool),
        last_verified: LAST_VERIFIED,
        minimum_version: min_version,
        protocol,
        supports_resume: true,
        supports_fork: supports_fork(tool),
        cost_coverage: cost,
        policy_coverage: policy,
        required_flags: flags,
        known_gaps: gaps,
        exit_code_trustworthy: exit_code_trustworthy(tool),
    })
}

/// Whether a tool's exit status may be believed over a terminal record that
/// certifies the turn as successful.
///
/// agy is the one preset whose own documentation calls its exit codes
/// unreliable, and sfh has trusted its envelope over its exit status since
/// before v1.2. Everything else - including a custom `cmd:`, which has no
/// protocol to weigh against - keeps the conservative reading: a non-zero exit
/// is a failure. A flow that knows better says so with `exit_conflict:`.
pub fn exit_code_trustworthy(tool: &str) -> bool {
    tool != "agy"
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
        //
        // --no-context-files (P0-02) belongs on read AND write, not read
        // alone: AGENTS.md/CLAUDE.md on disk and pi's SYSTEM/APPEND_SYSTEM
        // path are an unaudited prompt input regardless of which tools are
        // registered, and the read-vs-write line is about which tools pi may
        // USE, not about whether a file neither sfh nor the flow author wrote
        // gets to add hidden instructions. full does not get the flag: full
        // already means the operator trusts this tool with everything, so
        // suppressing pi's normal project-context behavior there would be an
        // undocumented narrowing of the one tier meant to hold nothing back.
        Access::Read => push(
            a,
            &[
                "--no-extensions",
                "--no-skills",
                "--no-prompt-templates",
                "--no-context-files",
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
                    "--no-context-files",
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

/// Boolean flags that mean "approve everything" on any tool. Matched as a WHOLE
/// argv element (or its --flag=... form) - a prefix match would turn a harmless
/// --force-of-nature into a bypass.
const GLOBAL_BYPASS_FLAGS: [&str; 5] = [
    "--dangerously-skip-permissions",
    "--dangerously-bypass-approvals-and-sandbox",
    "--always-approve",
    "--yolo",
    "--force",
];

/// The first arg that widens permissions past `access`, with the reason.
///
/// The check is VALUE-AWARE on purpose: `args: ["-c", "sandbox_mode=read-only"]`
/// on a read step NARROWS the permission and must pass, while the old substring
/// match refused it and then suggested `access: full` - pointing the user the
/// wrong way. Only args that raise the tier above the declared one are
/// reported; a value sfh cannot classify is let through, because a false
/// positive stops a correct flow while a miss is still caught by the tool's
/// own sandbox. A step that is not `full` may carry a widening arg only with an
/// explicit allow_access_override: true (enforced by the caller, fail-closed).
pub struct Escalation {
    /// The offending arg, plus its value when that is a separate element.
    pub arg: String,
    /// Why this widens the declared permission.
    pub reason: String,
}

/// Privilege tier of a value, on the same scale as `Access::rank`
/// (0 = read, 1 = write, 2 = full). `None` = cannot classify: let through.
fn tier_name(rank: u8) -> &'static str {
    match rank {
        0 => "read",
        1 => "write",
        _ => "full",
    }
}

fn wider(arg: &str, reason: String, value_rank: u8, access: Access) -> Option<Escalation> {
    if value_rank > access.rank() {
        Some(Escalation {
            arg: arg.to_string(),
            reason,
        })
    } else {
        None
    }
}

fn codex_sandbox_rank(mode: &str) -> Option<u8> {
    match mode.trim().trim_matches('"').trim() {
        "read-only" => Some(0),
        "workspace-write" => Some(1),
        "danger-full-access" => Some(2),
        _ => None,
    }
}

fn permission_mode_rank(mode: &str) -> Option<u8> {
    // claude and grok share the mode vocabulary.
    match mode.trim().trim_matches('"').trim() {
        "plan" | "dontAsk" | "default" => Some(0),
        "acceptEdits" => Some(1),
        "bypassPermissions" => Some(2),
        _ => None,
    }
}

fn unquote(v: &str) -> String {
    v.trim().trim_matches('"').trim().to_string()
}

/// Split a codex `-c`/`--config` value into a (key, unquoted-value) pair, e.g.
/// `sandbox_mode="danger-full-access"` -> ("sandbox_mode", "danger-full-access").
fn config_kv(value: &str) -> Option<(String, String)> {
    let (k, v) = value.split_once('=')?;
    let k = k.trim().to_string();
    if k.is_empty() {
        return None;
    }
    Some((k, unquote(v)))
}

pub fn find_escalation(tool: &str, access: Access, args: &[String]) -> Option<Escalation> {
    // IMPORTANT: a SAFE (narrowing or same-tier) arg must NOT stop the scan. The
    // old code did `return wider(...)` directly, so the first classifiable arg
    // ended the whole walk: `["--permission-mode","dontAsk",
    // "--dangerously-skip-permissions"]` classified `dontAsk` as read-tier,
    // returned None, and never looked at the full-access flag that followed.
    // Every branch now only returns when it finds a WIDENING arg and otherwise
    // falls through to the next element (rev_break #8).
    for (i, a) in args.iter().enumerate() {
        // Tool-independent "approve everything" switches.
        let (flag, eq_value) = match a.split_once('=') {
            Some((f, v)) => (f.to_string(), Some(v.to_string())),
            None => (a.clone(), None),
        };
        if GLOBAL_BYPASS_FLAGS.contains(&flag.as_str()) && access != Access::Full {
            return Some(Escalation {
                arg: a.clone(),
                reason: "bypasses all permission checks (= full access)".to_string(),
            });
        }
        let value_of = |takes_value: bool| -> Option<String> {
            eq_value
                .clone()
                .or_else(|| takes_value.then(|| args.get(i + 1).cloned()).flatten())
        };
        match tool {
            "codex" => {
                // -s/--sandbox <mode>, a bare -c value element sandbox_mode=<mode>,
                // OR the joined forms -c sandbox_mode=<mode> / --config=sandbox_mode=<mode>.
                let mode = if flag == "-s" || flag == "--sandbox" {
                    value_of(true)
                } else if flag == "sandbox_mode" {
                    Some(eq_value.clone().unwrap_or_default())
                } else if flag == "-c" || flag == "--config" {
                    value_of(true).and_then(|v| {
                        config_kv(&v).and_then(|(k, val)| (k == "sandbox_mode").then_some(val))
                    })
                } else {
                    None
                };
                if let Some(m) = mode {
                    let clean = unquote(&m);
                    if let Some(r) = codex_sandbox_rank(&clean) {
                        if let Some(e) = wider(
                            a,
                            format!(
                                "sandbox mode '{clean}' is the {} tier, above the declared '{}'",
                                tier_name(r),
                                access.as_str()
                            ),
                            r,
                            access,
                        ) {
                            return Some(e);
                        }
                    }
                }
                // approval_policy=<policy>: anything except "never" auto-approves
                // at least some actions. Recognised both as a bare -c value element
                // and inside the joined -c/--config forms.
                let approval = if flag == "approval_policy" {
                    eq_value.clone()
                } else if flag == "-c" || flag == "--config" {
                    value_of(true).and_then(|v| {
                        config_kv(&v).and_then(|(k, val)| (k == "approval_policy").then_some(val))
                    })
                } else {
                    None
                };
                if let Some(v) = approval {
                    let clean = unquote(&v);
                    let rank = match clean.as_str() {
                        "never" => Some(0),
                        "on-failure" | "on-request" | "unapproved" => Some(2),
                        _ => None,
                    };
                    if let Some(r) = rank {
                        if let Some(e) = wider(
                            a,
                            format!("approval_policy '{clean}' auto-approves tool calls"),
                            r,
                            access,
                        ) {
                            return Some(e);
                        }
                    }
                }
            }
            "claude" => {
                if flag == "--permission-mode" {
                    if let Some(v) = value_of(true) {
                        let clean = unquote(&v);
                        if let Some(r) = permission_mode_rank(&clean) {
                            if let Some(e) = wider(
                                a,
                                format!(
                                    "permission mode '{clean}' is the {} tier, above the declared '{}'",
                                    tier_name(r),
                                    access.as_str()
                                ),
                                r,
                                access,
                            ) {
                                return Some(e);
                            }
                        }
                    }
                } else if flag == "--tools" || flag == "--allowedTools" {
                    if let Some(v) = value_of(true) {
                        let r = claude_tool_list_rank(&v);
                        if let Some(e) = wider(
                            a,
                            format!(
                                "the tool list grants the {} tier, above the declared '{}'",
                                tier_name(r),
                                access.as_str()
                            ),
                            r,
                            access,
                        ) {
                            return Some(e);
                        }
                    }
                } else if flag == "--add-dir" {
                    if let Some(e) = wider(
                        a,
                        "grants access to directories outside the workspace".to_string(),
                        2,
                        access,
                    ) {
                        return Some(e);
                    }
                }
            }
            "opencode" => {
                // The agent selects the permission set; "build" is the known
                // write tier. A custom agent could be anything, so anything
                // else is let through (a miss is still the user's own config).
                if flag == "--agent" {
                    if let Some(v) = value_of(true) {
                        let clean = unquote(&v);
                        if clean == "build" {
                            if let Some(e) = wider(
                                a,
                                "the build agent edits files, above the declared 'read'"
                                    .to_string(),
                                1,
                                access,
                            ) {
                                return Some(e);
                            }
                        }
                    }
                }
            }
            "grok" => {
                if flag == "--permission-mode" {
                    if let Some(v) = value_of(true) {
                        let clean = unquote(&v);
                        if let Some(r) = permission_mode_rank(&clean) {
                            if let Some(e) = wider(
                                a,
                                format!(
                                    "permission mode '{clean}' is the {} tier, above the declared '{}'",
                                    tier_name(r),
                                    access.as_str()
                                ),
                                r,
                                access,
                            ) {
                                return Some(e);
                            }
                        }
                    }
                } else if flag == "--allow" {
                    if let Some(v) = value_of(true) {
                        let head = v
                            .split(['(', ':', '='])
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_lowercase();
                        let r = match head.as_str() {
                            "bash" => 2,
                            "edit" | "write" => 1,
                            _ => 0,
                        };
                        if let Some(e) = wider(
                            a,
                            format!(
                                "allowing '{v}' grants the {} tier, above the declared '{}'",
                                tier_name(r),
                                access.as_str()
                            ),
                            r,
                            access,
                        ) {
                            return Some(e);
                        }
                    }
                }
            }
            "agy" => {
                if flag == "--mode" {
                    if let Some(v) = value_of(true) {
                        let clean = unquote(&v);
                        let rank = match clean.as_str() {
                            "plan" => Some(0),
                            "accept-edits" => Some(1),
                            _ => None,
                        };
                        if let Some(r) = rank {
                            if let Some(e) = wider(
                                a,
                                format!(
                                    "mode '{clean}' is the {} tier, above the declared '{}'",
                                    tier_name(r),
                                    access.as_str()
                                ),
                                r,
                                access,
                            ) {
                                return Some(e);
                            }
                        }
                    }
                }
            }
            "pi" => {
                if flag == "--approve" && access != Access::Full {
                    return Some(Escalation {
                        arg: a.clone(),
                        reason: "auto-approves every action (= full access)".to_string(),
                    });
                }
                if flag == "--tools" || flag == "-t" {
                    if let Some(v) = value_of(true) {
                        let r = pi_tool_list_rank(&v);
                        if let Some(e) = wider(
                            a,
                            format!(
                                "the tool list grants the {} tier, above the declared '{}'",
                                tier_name(r),
                                access.as_str()
                            ),
                            r,
                            access,
                        ) {
                            return Some(e);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// claude --tools/--allowedTools: Bash is a shell (= full), edit-family tools
/// are the write tier, anything else (Read, Grep, WebFetch...) stays read.
fn claude_tool_list_rank(list: &str) -> u8 {
    let tools: Vec<String> = list
        .split(',')
        .map(|t| t.trim().trim_matches('"').to_lowercase())
        .collect();
    if tools.iter().any(|t| t.starts_with("bash")) {
        2
    } else if tools
        .iter()
        .any(|t| matches!(t.as_str(), "edit" | "write" | "multiedit" | "notebookedit"))
    {
        1
    } else {
        0
    }
}

/// pi --tools/-t: bash is full, edit/write are the write tier.
fn pi_tool_list_rank(list: &str) -> u8 {
    let tools: Vec<String> = list
        .split(',')
        .map(|t| t.trim().trim_matches('"').to_lowercase())
        .collect();
    if tools.iter().any(|t| t == "bash") {
        2
    } else if tools.iter().any(|t| t == "edit" || t == "write") {
        1
    } else {
        0
    }
}

/// The shared error text for a widening arg. Deliberately does NOT suggest
/// `access: full`: the right fix is usually to remove or narrow the arg, and
/// pointing at full would escalate the flow in the wrong direction.
pub fn escalation_error(step_id: &str, access: Access, e: &Escalation) -> String {
    format!(
        "step '{step_id}': args: contains '{}', which overrides the declared access level ({}). Remove the arg or keep it within access: {}, or set allow_access_override: true on this step to accept it",
        e.arg,
        e.reason,
        access.as_str()
    )
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

/// The enforcement config opencode receives, built as a JSON *value* and then
/// serialized (spec P0-03).
///
/// The agent name is user-controlled (`agent:` on a step, a profile, or a
/// `--var`-rendered value). Interpolating it into a JSON string literal with
/// `format!` meant a name containing a quote or a backslash produced either
/// invalid JSON - in which case opencode ignores the layer and the deny rules
/// silently vanish - or, worse, valid JSON with an attacker-chosen structure.
/// `serde_json` escapes the key for us, so no name can do either.
fn opencode_env(agent_name: &str, access: Access) -> Vec<(String, String)> {
    let config = match access {
        // The stock plan agent denies edits but NOT bash (1.18.3), so a read step
        // could still write via shell redirection. OPENCODE_CONFIG_CONTENT is the
        // highest-precedence config layer and merges with the user's config.
        Access::Read => serde_json::json!({
            "agent": { agent_name: { "permission": { "edit": "deny", "bash": "deny" } } }
        }),
        // --auto auto-approves whatever is not explicitly denied, so write denies
        // two things: bash (opencode has no OS sandbox - an auto-approved shell
        // would make write == full, the same rule pi/claude/grok follow) and
        // out-of-tree writes. Agent-scoped so a user-selected agent cannot
        // inherit looser defaults.
        Access::Write => serde_json::json!({
            "agent": { agent_name: { "permission": { "bash": "deny" } } },
            "permission": { "external_directory": "deny" }
        }),
        Access::Full => return Vec::new(),
    };
    vec![("OPENCODE_CONFIG_CONTENT".to_string(), config.to_string())]
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

/// The token codex's own `--help` has to contain for sfh to trust `exec fork`
/// on the installed binary (P1-07). Deliberately NOT folded into
/// `AdapterInfo.required_flags`, which `preflight` checks on every run
/// regardless of what the step asked for: a codex whose `--help` stays silent
/// about `fork` is still a perfectly good codex for a fresh run or a
/// `continue_from` resume, so refusing those too would be exactly the
/// per-adapter-vs-per-capability mismatch this function exists to avoid.
/// `None` (the caller could not read `--help` at all) is exactly as
/// untrustworthy as help text that never mentions fork, so both fail closed
/// the same way.
fn codex_fork_confirmed(installed_help: Option<&str>) -> bool {
    installed_help.is_some_and(|h| h.contains("fork"))
}

/// Build the command line to FORK a session: the child inherits the parent's
/// history but writes to its own session, so N children can diverge from one
/// context concurrently without corrupting each other or the parent.
///
/// Every supporting tool refuses loudly (exit 1, before any model call) when
/// the parent id does not exist, so - unlike pi's create-or-resume --session-id -
/// a fork cannot silently degrade into a cold session. The remaining risk is the
/// fork flag being ignored, which the caller detects by requiring child != parent.
///
/// `installed_help` is the resolved binary's own `--help` output, when the
/// caller could read it. It is consulted ONLY by codex's arm: `exec fork`
/// shipped after this file's `LAST_VERIFIED` baseline, so `supports_fork`
/// returning `true` for codex is an adapter-wide fact, not proof THIS
/// installed build has ever heard of the subcommand - see
/// `codex_fork_confirmed`. Every other tool's fork predates that baseline and
/// ignores this argument.
pub fn build_fork(
    tool: &str,
    parent_session_id: &str,
    child_session_id: &str,
    inp: PresetInput,
    paths: &BuildPaths,
    installed_help: Option<&str>,
) -> Result<Built, String> {
    let mut a: Vec<String> = Vec::new();
    let mut warnings = Vec::new();
    let mut env_remove = Vec::new();
    let mut env_set = Vec::new();
    let parse;
    let delivery;
    // opencode and codex mint the child id themselves; the others let sfh
    // name it.
    let mut preassigned = Some(child_session_id.to_string());
    match tool {
        "codex" => {
            // sfh's belief that codex CAN fork is adapter-wide (supports_fork);
            // whether THIS installed binary has ever heard of the subcommand
            // is not, so launching blind risks the exact failure this gate
            // exists to avoid: an older codex either erroring in a way sfh
            // cannot tell apart from a real failure, or silently treating
            // "fork" as an ordinary argument and spending a real turn on a
            // request nobody made.
            if !codex_fork_confirmed(installed_help) {
                return Err(format!(
                    "codex needs a build that recognises 'exec fork' to branch a session headlessly, and sfh {}. Run 'codex exec --help' yourself to check, upgrade codex if it is missing, or use continue_from to chain this step serially instead",
                    if installed_help.is_some() {
                        "could read its --help but it never mentions fork"
                    } else {
                        "could not confirm this from the installed binary"
                    }
                ));
            }
            push(&mut a, &["codex", "exec", "fork"]);
            a.push(parent_session_id.to_string());
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
            // Like `exec resume`, `exec fork` has no -s flag - rebuild the
            // sandbox via -c instead of assuming the child inherits the
            // parent's.
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
            if inp.agent.is_some() {
                warnings.push("codex preset ignores 'agent' (no --agent flag in exec)".into());
            }
            a.extend(inp.extra.iter().cloned());
            push(&mut a, &["--output-last-message"]);
            a.push(paths.last_msg.display().to_string());
            push(&mut a, &["-"]);
            // codex mints the child's session id itself and reports it via
            // thread.started in the fork run's own JSONL, exactly like a
            // fresh run - sfh cannot preassign it (codex is absent from
            // wants_preassign for the same reason).
            preassigned = None;
            parse = OutputParse::CodexJsonl(paths.last_msg.to_path_buf());
            delivery = Delivery::Stdin;
        }
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
                ["codex", "claude", "opencode", "grok", "pi"].join("/")
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

    #[test]
    fn usage_accumulates_attempts_and_rejects_refunds() {
        let mut total = Usage {
            input_tokens: Some(u64::MAX),
            output_tokens: Some(2),
            cost_usd: Some(0.10),
            invalid_cost: false,
        };
        total.accumulate(&Usage {
            input_tokens: Some(1),
            output_tokens: Some(3),
            cost_usd: Some(0.20),
            invalid_cost: false,
        });
        assert_eq!(total.input_tokens, Some(u64::MAX));
        assert_eq!(total.output_tokens, Some(5));
        assert!((total.reported_cost() - 0.30).abs() < f64::EPSILON);

        total.accumulate(&Usage {
            input_tokens: None,
            output_tokens: None,
            cost_usd: Some(-99.0),
            invalid_cost: false,
        });
        assert!((total.reported_cost() - 0.30).abs() < f64::EPSILON);
        assert!(
            total.sanitize_reported(),
            "the invalid component is retained"
        );

        let mut streamed = Usage::default();
        streamed.add_reported_cost(0.10);
        streamed.add_reported_cost(-0.10);
        assert_eq!(streamed.reported_cost(), 0.10);
        assert!(streamed.sanitize_reported());
    }

    #[test]
    fn usage_normalizes_non_finite_costs_fail_closed() {
        for invalid in [-1.0, f64::NEG_INFINITY, f64::NAN] {
            let mut usage = Usage {
                cost_usd: Some(invalid),
                ..Default::default()
            };
            assert!(usage.sanitize_reported());
            assert_eq!(usage.cost_usd, Some(0.0));
        }
        let mut infinite = Usage {
            cost_usd: Some(f64::INFINITY),
            ..Default::default()
        };
        assert!(infinite.sanitize_reported());
        assert_eq!(infinite.cost_usd, Some(f64::MAX));
    }
    use std::path::PathBuf;

    fn inp(access: Access, extra: &[String]) -> PresetInput<'_> {
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

    #[test]
    fn access_parse_rejects_unknown_values() {
        // rev_complete S2-4: an unparseable access string (e.g. a log.jsonl an
        // attacker edited to "access":"bogus") must NOT parse to a usable level.
        // engine::load_resume maps the Err to None via .ok(), and the resume
        // guard then fails closed on None - this test pins the first link.
        assert_eq!(Access::parse(Some("read")).unwrap(), Access::Read);
        assert_eq!(Access::parse(Some("write")).unwrap(), Access::Write);
        assert_eq!(Access::parse(Some("full")).unwrap(), Access::Full);
        assert!(Access::parse(Some("bogus")).is_err());
        assert!(Access::parse(Some("FULL")).is_err());
        assert!(Access::parse(Some("")).is_err());
        // None defaults to write (cmd: steps), never to an escalation.
        assert_eq!(Access::parse(None).unwrap(), Access::Write);
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

    /// codex's own `--help`, shaped enough to convince `codex_fork_confirmed`
    /// that this installed build has heard of `exec fork` - used wherever a
    /// test needs codex to actually clear the P1-07 capability gate.
    const CODEX_HELP_WITH_FORK: &str = "usage: codex exec [OPTIONS] [PROMPT]\n\nSUBCOMMANDS:\n    resume    Resume a previous session\n    fork      Fork a previous session into a new one\n";

    #[test]
    fn fork_builds_a_child_session_for_every_supporting_tool() {
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        for t in ["claude", "opencode", "grok", "pi", "codex"] {
            assert!(supports_fork(t), "{t}");
            // codex additionally needs live proof the installed binary knows
            // about `exec fork` (P1-07; see build_fork's codex arm) before it
            // will build anything - every other tool's fork is unconditional
            // once supports_fork says yes, so this argument is None for them.
            let help = (t == "codex").then_some(CODEX_HELP_WITH_FORK);
            let b = build_fork(t, "PARENT", "CHILD", inp(Access::Read, &[]), &bp, help).unwrap();
            assert!(
                b.argv.iter().any(|x| x == "PARENT"),
                "{t}: parent id missing"
            );
            match t {
                // opencode and codex mint the child id themselves; it cannot
                // be named up front the way claude/grok/pi's can.
                "opencode" => {
                    assert!(b.preassigned_session.is_none());
                    assert!(b.argv.iter().any(|x| x == "--fork"));
                }
                "codex" => {
                    assert!(b.preassigned_session.is_none());
                    assert!(b.argv.windows(2).any(|w| w[0] == "exec" && w[1] == "fork"));
                }
                _ => {
                    assert_eq!(b.preassigned_session.as_deref(), Some("CHILD"), "{t}");
                    assert!(b.argv.iter().any(|x| x == "CHILD"), "{t}");
                }
            }
            // A fork must never be asserted to equal the parent session.
            assert!(b.expect_session.is_none(), "{t}");
        }
        for t in ["agy", "cursor"] {
            assert!(!supports_fork(t), "{t}");
            let e = build_fork(t, "P", "C", inp(Access::Read, &[]), &bp, None)
                .err()
                .unwrap();
            assert!(e.contains("cannot fork"), "{t}: {e}");
        }
    }

    /// P1-07. `supports_fork("codex")` is an adapter-wide fact; it must not by
    /// itself be enough to spend a real `exec fork` call. Neither a probe that
    /// failed outright (`None`) nor one that succeeded but never mentions fork
    /// (an old codex whose `--help` only knows about `resume`) is trusted, and
    /// the refusal has to name what an operator can actually do about it.
    #[test]
    fn codex_fork_refuses_an_older_or_unknown_codex_and_says_what_to_do() {
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        for help in [
            None,
            Some("usage: codex exec resume [OPTIONS] SESSION_ID\n"),
        ] {
            let e = build_fork(
                "codex",
                "parent-1",
                "child-1",
                inp(Access::Read, &[]),
                &bp,
                help,
            )
            .err()
            .unwrap_or_else(|| panic!("help={help:?} must not be trusted to fork"));
            assert!(e.contains("exec fork"), "{e}");
            assert!(
                e.contains("upgrade") && e.contains("continue_from"),
                "the refusal must name what to do next: {e}"
            );
        }
    }

    /// P1-07. Once the capability gate clears, codex's fork argv has to
    /// follow the same shape as its resume: the subcommand form (not
    /// --session-id/-r, which codex does not have), no -s (rebuilt via -c),
    /// and the prompt on stdin.
    #[test]
    fn codex_fork_rebuilds_the_sandbox_like_resume_and_reads_stdin() {
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        let b = build_fork(
            "codex",
            "parent-1",
            "child-1",
            inp(Access::Write, &[]),
            &bp,
            Some(CODEX_HELP_WITH_FORK),
        )
        .unwrap();
        assert!(b.argv.windows(2).any(|w| w[0] == "exec" && w[1] == "fork"));
        assert!(b.argv.iter().any(|x| x == "parent-1"), "{:?}", b.argv);
        assert!(
            !b.argv.iter().any(|x| x == "-s"),
            "exec fork has no -s flag, same as exec resume: {:?}",
            b.argv
        );
        assert!(b
            .argv
            .iter()
            .any(|x| x == "sandbox_mode=\"workspace-write\""));
        assert_eq!(
            b.argv.last().unwrap(),
            "-",
            "codex reads the prompt from stdin"
        );
    }

    #[test]
    fn codex_is_not_cwd_scoped_for_fork_the_same_way_it_is_not_for_resume() {
        // codex looks sessions up by thread id alone; see fork_is_cwd_scoped's
        // and session_is_cwd_scoped's doc comments for why this is deliberate,
        // not an oversight.
        assert!(!fork_is_cwd_scoped("codex"));
        assert!(!session_is_cwd_scoped("codex"));
    }

    #[test]
    fn only_claude_warms_up_by_default() {
        assert!(fork_warmup_pays("claude"));
        for t in ["opencode", "grok", "pi", "codex"] {
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
                    None,
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

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // The audit found seven strings checked by naive contains: pi's -t (which
    // the README itself suggested for adding Bash), claude's --allowedTools,
    // codex's -c sandbox_mode=... and friends all slipped through. Each tool's
    // permission levers must be caught - and the check must look at the VALUE,
    // so an arg that NARROWS the permission (sandbox_mode=read-only on a read
    // step) is not a false positive.
    #[test]
    fn escalation_detection_covers_each_tools_permission_levers() {
        for (tool, access, argv) in [
            ("pi", Access::Read, args(&["--approve"])),
            ("pi", Access::Read, args(&["--tools", "read,edit"])),
            (
                "pi",
                Access::Write,
                args(&["-t", "read,bash,edit,write,grep,find,ls"]),
            ),
            ("opencode", Access::Read, args(&["--agent", "build"])),
            ("opencode", Access::Read, args(&["--agent=build"])),
            ("claude", Access::Read, args(&["--tools", "Read,Bash"])),
            ("claude", Access::Read, args(&["--allowedTools", "Bash"])),
            (
                "claude",
                Access::Write,
                args(&["--allowedTools", "Bash(ls)"]),
            ),
            (
                "claude",
                Access::Write,
                args(&["--permission-mode", "bypassPermissions"]),
            ),
            (
                "claude",
                Access::Read,
                args(&["--permission-mode=acceptEdits"]),
            ),
            ("claude", Access::Read, args(&["--add-dir", "/etc"])),
            ("grok", Access::Write, args(&["--allow", "Bash(ls)"])),
            (
                "grok",
                Access::Read,
                args(&["--permission-mode", "bypassPermissions"]),
            ),
            ("agy", Access::Read, args(&["--mode", "accept-edits"])),
            ("codex", Access::Write, args(&["-s", "danger-full-access"])),
            (
                "codex",
                Access::Read,
                args(&["--sandbox", "workspace-write"]),
            ),
            (
                "codex",
                Access::Read,
                args(&["-c", "sandbox_mode=\"danger-full-access\""]),
            ),
            // a bare -c value element, the way args: actually carries it
            (
                "codex",
                Access::Read,
                args(&["sandbox_mode=\"workspace-write\""]),
            ),
            (
                "codex",
                Access::Read,
                args(&["-c", "approval_policy=\"on-failure\""]),
            ),
            ("codex", Access::Read, args(&["approval_policy=on-request"])),
            // tool-independent bypasses
            ("claude", Access::Write, args(&["--force"])),
            ("codex", Access::Read, args(&["--yolo"])),
            (
                "grok",
                Access::Read,
                args(&["--dangerously-skip-permissions"]),
            ),
            ("agy", Access::Write, args(&["--always-approve"])),
            (
                "codex",
                Access::Write,
                args(&["--dangerously-bypass-approvals-and-sandbox"]),
            ),
        ] {
            assert!(
                find_escalation(tool, access, &argv).is_some(),
                "{tool} at {:?}: {:?} slipped through",
                access,
                argv
            );
        }
    }

    #[test]
    fn escalation_scan_does_not_stop_at_a_safe_arg() {
        // rev_break #8: a SAFE (narrowing / same-tier) arg used to end the whole
        // scan, so a widening arg placed AFTER a harmless one slipped through.
        // dontAsk is the read tier on a read step (safe), but the Bash tool list
        // that follows it is full access and must still be caught.
        assert!(
            find_escalation(
                "claude",
                Access::Read,
                &args(&["--permission-mode", "dontAsk", "--allowedTools", "Bash"])
            )
            .is_some(),
            "a widening arg after a safe one must not be skipped"
        );
        // Same shape for codex: a narrowing sandbox first, then a full-access one.
        assert!(find_escalation(
            "codex",
            Access::Read,
            &args(&["-s", "read-only", "-s", "danger-full-access"])
        )
        .is_some());
    }

    #[test]
    fn escalation_detects_joined_config_sandbox_mode() {
        // rev_break #8: --config=sandbox_mode="danger-full-access" is a single
        // argv element; the old code only recognised sandbox_mode as its own
        // element (or -s/--sandbox), so the joined form was invisible.
        assert!(find_escalation(
            "codex",
            Access::Read,
            &args(&["--config=sandbox_mode=\"danger-full-access\""])
        )
        .is_some());
        assert!(find_escalation(
            "codex",
            Access::Read,
            &args(&["-c", "approval_policy=on-failure"])
        )
        .is_some());
        // The joined form still narrows correctly.
        assert!(find_escalation(
            "codex",
            Access::Read,
            &args(&["--config=sandbox_mode=\"read-only\""])
        )
        .is_none());
    }

    #[test]
    fn escalation_detection_lets_narrowing_and_harmless_args_through() {
        for (tool, access, argv) in [
            // THE false positives from the review: these NARROW the permission.
            (
                "codex",
                Access::Read,
                args(&["-c", "sandbox_mode=read-only"]),
            ),
            (
                "codex",
                Access::Read,
                args(&["-c", "sandbox_mode=\"read-only\""]),
            ),
            ("codex", Access::Read, args(&["-s", "read-only"])),
            ("codex", Access::Write, args(&["-s", "workspace-write"])),
            (
                "codex",
                Access::Write,
                args(&["-c", "sandbox_mode=\"read-only\""]),
            ),
            (
                "codex",
                Access::Read,
                args(&["-c", "approval_policy=\"never\""]),
            ),
            ("codex", Access::Read, args(&["approval_policy=never"])),
            (
                "claude",
                Access::Write,
                args(&["--permission-mode", "acceptEdits"]),
            ),
            (
                "claude",
                Access::Read,
                args(&["--permission-mode", "dontAsk"]),
            ),
            // a prose value that merely MENTIONS a bypass word is not a bypass
            (
                "claude",
                Access::Read,
                args(&["--append-system-prompt", "never use bypassPermissions"]),
            ),
            (
                "claude",
                Access::Read,
                args(&["--allowedTools", "WebSearch,WebFetch"]),
            ),
            (
                "claude",
                Access::Write,
                args(&["--allowedTools", "Edit,Write"]),
            ),
            (
                "grok",
                Access::Write,
                args(&["--permission-mode", "acceptEdits"]),
            ),
            ("grok", Access::Read, args(&["--deny", "Bash"])),
            ("agy", Access::Write, args(&["--mode", "plan"])),
            ("agy", Access::Read, args(&["--mode", "plan"])),
            ("pi", Access::Read, args(&["-t", "read,grep,find,ls"])),
            (
                "pi",
                Access::Write,
                args(&["--tools", "read,edit,write,grep,find,ls"]),
            ),
            ("opencode", Access::Write, args(&["--agent", "build"])),
            ("opencode", Access::Read, args(&["--agent", "plan"])),
            // a custom agent could be anything: ambiguous args are let through
            (
                "opencode",
                Access::Read,
                args(&["--agent", "my-custom-agent"]),
            ),
            ("codex", Access::Read, args(&["-s", "some-future-mode"])),
            ("agy", Access::Read, args(&["--mode", "unknown-mode"])),
            // plain settings and prefix lookalikes
            ("codex", Access::Read, args(&["--model", "gpt"])),
            ("codex", Access::Read, args(&["-m", "gpt"])),
            (
                "codex",
                Access::Read,
                args(&["-c", "model_reasoning_effort=\"high\""]),
            ),
            ("claude", Access::Read, args(&["--effort", "high"])),
            ("claude", Access::Read, args(&["--output-format", "json"])),
            ("opencode", Access::Read, args(&["--variant", "high"])),
            ("grok", Access::Read, args(&["--reasoning-effort", "high"])),
            ("agy", Access::Read, args(&["--model", "gemini"])),
            ("agy", Access::Read, args(&["--agent", "x"])),
            ("pi", Access::Read, args(&["--thinking", "high"])),
            ("pi", Access::Read, args(&["--no-approve"])),
            ("pi", Access::Read, args(&["--no-extensions"])),
            ("pi", Access::Read, args(&["--session-id", "abc"])),
            ("cursor", Access::Read, args(&["--mode", "plan"])),
            ("cursor", Access::Read, args(&["--trust"])),
            ("codex", Access::Read, args(&["--force-of-nature"])),
            ("agy", Access::Read, args(&["--modest"])),
        ] {
            assert!(
                find_escalation(tool, access, &argv).is_none(),
                "{tool} at {:?}: {:?} is a false positive",
                access,
                argv
            );
        }
        // full may carry anything
        assert!(
            find_escalation("codex", Access::Full, &args(&["-s", "danger-full-access"])).is_none()
        );
        assert!(find_escalation("pi", Access::Full, &args(&["--approve"])).is_none());
        assert!(find_escalation("claude", Access::Full, &args(&["--force"])).is_none());
    }

    #[test]
    fn escalation_error_points_at_removal_not_at_full() {
        assert!(find_escalation(
            "codex",
            Access::Read,
            &args(&["-c", "sandbox_mode=read-only"])
        )
        .is_none());
        let bad = find_escalation("codex", Access::Read, &args(&["-s", "danger-full-access"]))
            .expect("danger-full-access on a read step is an escalation");
        let msg = escalation_error("safe", Access::Read, &bad);
        assert!(msg.contains("overrides the declared access level"), "{msg}");
        assert!(msg.contains("allow_access_override"), "{msg}");
        assert!(msg.contains("access: read"), "{msg}");
        // The old message told users to reach for access: full - the exact
        // wrong direction. The fix is to remove or narrow the arg.
        assert!(!msg.contains("use access: full"), "{msg}");
    }

    /// P0-03. The agent name is flow data, so it can hold a quote, a backslash
    /// or a control character. Building the permission config by pasting it
    /// into a JSON string literal produced either invalid JSON - which opencode
    /// discards, silently dropping the deny rules - or attacker-chosen
    /// structure. Every name must come back as valid JSON whose deny rules sit
    /// under exactly one agent key holding exactly the requested name.
    #[test]
    fn opencode_enforcement_config_is_valid_json_for_any_agent_name() {
        let hostile = [
            "plain",
            "with\"quote",
            "with\\backslash",
            "with\u{0007}control\u{0000}nul",
            "with\nnewline",
            // The shape a string-concatenating builder would let escape: close
            // the key, close the objects, and append a permission of one's own.
            r#"x":{"permission":{"bash":"allow"}}},"a":{"y"#,
            r#"}}},"permission":{"external_directory":"allow"},"junk":{"#,
            "日本語のエージェント",
        ];
        for name in hostile {
            for (access, denied) in [
                (Access::Read, vec!["edit", "bash"]),
                (Access::Write, vec!["bash"]),
            ] {
                let env = opencode_env(name, access);
                let (key, raw) = env.first().expect("read/write must set the config layer");
                assert_eq!(key, "OPENCODE_CONFIG_CONTENT");
                let v: serde_json::Value = serde_json::from_str(raw)
                    .unwrap_or_else(|e| panic!("agent {name:?} produced invalid JSON: {e}\n{raw}"));
                let agents = v
                    .get("agent")
                    .and_then(|a| a.as_object())
                    .unwrap_or_else(|| panic!("agent {name:?}: no agent map in {raw}"));
                assert_eq!(
                    agents.len(),
                    1,
                    "agent {name:?} injected extra agent keys: {raw}"
                );
                let perms = agents
                    .get(name)
                    .and_then(|a| a.get("permission"))
                    .and_then(|p| p.as_object())
                    .unwrap_or_else(|| panic!("agent {name:?}: name is not the key in {raw}"));
                for d in &denied {
                    assert_eq!(
                        perms.get(*d).and_then(|x| x.as_str()),
                        Some("deny"),
                        "agent {name:?}: {d} must stay denied in {raw}"
                    );
                }
                assert!(
                    !perms.values().any(|x| x.as_str() == Some("allow")),
                    "agent {name:?} smuggled an allow into {raw}"
                );
                if access == Access::Write {
                    assert_eq!(
                        v.pointer("/permission/external_directory")
                            .and_then(|x| x.as_str()),
                        Some("deny"),
                        "agent {name:?}: out-of-tree writes must stay denied in {raw}"
                    );
                }
            }
            assert!(
                opencode_env(name, Access::Full).is_empty(),
                "full access sets no enforcement layer"
            );
        }
    }

    /// P0-02. `Enforced` used to mean "sfh denied the builtin tools its
    /// preset author enumerated"; the review found that bar too low for five
    /// adapters, because none of the five denials reach MCP tools, plugins,
    /// hooks, subagents or instruction files. agy was left at `Enforced` in
    /// that first pass only because the review did not name it, not because
    /// it was examined - a later pass applied the same bar and found agy's
    /// `--mode` is the identical bare mode switch cursor's and claude's
    /// already-downgraded modes are, so it joined them. Only a tool whose OWN
    /// flag is documented to bound the entire process (codex's sandbox)
    /// still earns `Enforced` today. This test pins the corrected table so a
    /// future edit cannot silently re-claim `Enforced` for an
    /// enumerated-denylist or bare-mode adapter without a reviewer noticing
    /// the diff.
    #[test]
    fn enforced_is_reserved_for_a_holistic_guarantee_not_an_enumerated_denylist() {
        let expected: &[(&str, Enforcement, Enforcement, Enforcement)] = &[
            (
                "codex",
                Enforcement::Sandboxed,
                Enforcement::Sandboxed,
                Enforcement::BestEffort,
            ),
            (
                "claude",
                Enforcement::BestEffort,
                Enforcement::BestEffort,
                Enforcement::BestEffort,
            ),
            (
                "opencode",
                Enforcement::BestEffort,
                Enforcement::BestEffort,
                Enforcement::BestEffort,
            ),
            (
                "grok",
                Enforcement::BestEffort,
                Enforcement::BestEffort,
                Enforcement::BestEffort,
            ),
            (
                "agy",
                Enforcement::BestEffort,
                Enforcement::BestEffort,
                Enforcement::BestEffort,
            ),
            (
                "pi",
                Enforcement::BestEffort,
                Enforcement::BestEffort,
                Enforcement::BestEffort,
            ),
            (
                "cursor",
                Enforcement::BestEffort,
                Enforcement::Unsupported,
                Enforcement::BestEffort,
            ),
        ];
        for (tool, read, write, full) in expected {
            let i = adapter_info(tool).unwrap();
            assert_eq!(i.enforcement(Access::Read), *read, "{tool} read");
            assert_eq!(i.enforcement(Access::Write), *write, "{tool} write");
            assert_eq!(i.enforcement(Access::Full), *full, "{tool} full");
        }
    }

    /// P0-02. A gap list that is merely non-empty but generic ("some things
    /// may not be covered") would pass a bare emptiness check and still tell
    /// an operator nothing they could act on - and a copy-pasted list is the
    /// same failure wearing a different tool name. Every adapter's list has
    /// to name its own concrete uncovered surfaces, and no two adapters may
    /// share one.
    #[test]
    fn known_gaps_name_concrete_surfaces_and_are_not_copy_pasted_across_adapters() {
        let mut seen: Vec<(&str, &[&str])> = Vec::new();
        for tool in crate::flow::TOOLS {
            let i = adapter_info(tool).unwrap();
            assert!(!i.known_gaps.is_empty(), "{tool} lists no known gaps");
            for (other_tool, other_gaps) in &seen {
                assert_ne!(
                    i.known_gaps, *other_gaps,
                    "{tool} and {other_tool} share an identical known_gaps list - that is a generic list wearing two names"
                );
            }
            seen.push((tool, i.known_gaps));
        }
        // Spot-check that the concrete surfaces the P0-02 review named for
        // each adapter actually made it into the list, so a future edit
        // cannot quietly water these back down to something generic.
        let must_mention: &[(&str, &[&str])] = &[
            ("claude", &["MCP", "plugin", "hook", "instruction"]),
            ("opencode", &["--auto", "task", "skill", "MCP", "plugin"]),
            ("grok", &["MCP", "sandbox", "auto-update", "plugin"]),
            ("agy", &["MCP", "plugin", "hook", "instruction", "sandbox"]),
            ("pi", &["SYSTEM", "APPEND_SYSTEM", "AGENTS.md"]),
            ("cursor", &["MCP", "rule"]),
        ];
        for (tool, needles) in must_mention {
            let i = adapter_info(tool).unwrap();
            let joined = i.known_gaps.join(" | ");
            for needle in *needles {
                assert!(
                    joined.contains(needle),
                    "{tool}'s known_gaps do not mention '{needle}': {joined}"
                );
            }
        }
    }

    /// P0-02. Pi documents --no-context-files, and the concrete gap it closes
    /// (AGENTS.md/CLAUDE.md loading unaudited into the prompt) applies to both
    /// restrictive tiers, not read alone - see the comment in pi_common. full
    /// is deliberately excluded: it already means the operator trusts pi with
    /// everything, so suppressing normal project context there would be an
    /// undocumented narrowing of the one tier meant to hold nothing back.
    #[test]
    fn pi_strict_presets_suppress_hidden_context_files() {
        assert!(build_argv("pi", Access::Read)
            .iter()
            .any(|x| x == "--no-context-files"));
        assert!(build_argv("pi", Access::Write)
            .iter()
            .any(|x| x == "--no-context-files"));
        assert!(
            !build_argv("pi", Access::Full)
                .iter()
                .any(|x| x == "--no-context-files"),
            "full trusts pi with everything; suppressing context files there would be a silent narrowing"
        );
    }

    /// P0-02. The review asked for --no-auto-update on every scripted grok
    /// invocation, but nothing in this codebase's prior research (unlike
    /// cursor's --disable-auto-update, which IS attested here) confirms that
    /// flag's name for the pinned grok CLI. Inventing a plausible-looking
    /// flag would repeat exactly the failure this whole fix is about: a
    /// guarantee sfh claims but never verified. The gap stays open and named
    /// in known_gaps instead, until someone confirms the real flag against
    /// grok's own --help or documentation.
    #[test]
    fn grok_does_not_claim_an_unconfirmed_auto_update_flag() {
        for access in [Access::Read, Access::Write, Access::Full] {
            let argv = build_argv("grok", access);
            assert!(
                !argv.iter().any(|x| x.contains("auto-update")),
                "grok argv claims an auto-update flag sfh never confirmed: {argv:?}"
            );
        }
        let i = adapter_info("grok").unwrap();
        assert!(
            i.known_gaps.iter().any(|g| g.contains("auto-update")),
            "the declined flag must stay a named, tracked gap: {:?}",
            i.known_gaps
        );
    }

    /// The other half of P1-06's drift problem, in the direction the sfh-side
    /// test above cannot see.
    ///
    /// Preflight blocks a run when a flag an adapter needs is missing from the
    /// binary's own `--help`. The shell suite points every preset at one stub
    /// binary, so that stub's help text has to stay a superset of every
    /// adapter's `required_flags` - and when the lists above were widened to
    /// what the builders really emit, it silently stopped being one. The
    /// failure that produces is maximally misleading: `tests/engine_behaviour.sh`
    /// reports a missing flag on a CLI that never had one, and nothing points
    /// at the fixture. Reading the stub's source here turns that into a unit
    /// test failure naming the exact flag, next to the list that caused it.
    #[test]
    fn the_session_stub_advertises_every_flag_an_adapter_requires() {
        // The stub is a separate binary, not part of this crate, so its source
        // is read as text rather than linked against.
        let stub = include_str!("../tests/stub/session_stub.rs");
        let help = stub
            .split_once("const STUB_HELP: &str = \"\\")
            .expect("the stub still defines STUB_HELP")
            .1
            .split_once("\";")
            .expect("STUB_HELP is still a single literal")
            .0;
        for tool in crate::flow::TOOLS {
            let i = adapter_info(tool).unwrap();
            for flag in i.required_flags {
                assert!(
                    help.split_whitespace().any(|w| w == *flag),
                    "{tool} requires {flag}, which tests/stub/session_stub.rs's STUB_HELP does not advertise - preflight will block the shell suite with a missing-flag blocker that has nothing to do with the CLI"
                );
            }
        }
    }

    /// P1-06. `required_flags` used to be hand-maintained and drifted from
    /// what the builders actually emit, so preflight's `--help` drift check
    /// could miss a flag the upstream CLI renamed or dropped simply because
    /// nobody had added it to this list. This walks every builder (fresh,
    /// resume, fork), every access level, with every optional field turned on
    /// so conditional flags (--model, --agent, ...) actually appear, and
    /// asserts every long ("--foo") flag emitted is named in that adapter's
    /// required_flags. It deliberately ignores short flags (-s, -c, -p, ...)
    /// and bare subcommands (exec, run): the drift this guards against is a
    /// long flag silently going unlisted, and those are the ones `preflight`
    /// is most likely to have never had a reason to name explicitly.
    #[test]
    fn required_flags_names_every_long_flag_a_builder_can_emit() {
        let (l, p) = paths();
        let bp = BuildPaths {
            last_msg: &l,
            prompt_file: &p,
        };
        let full_inp = |access: Access| PresetInput {
            model: Some("test-model".to_string()),
            effort: Some("high".to_string()),
            access,
            agent: Some("test-agent".to_string()),
            extra: &[],
            bin: None,
            timeout_sec: Some(900),
        };
        let long_flags = |argv: &[String]| -> Vec<String> {
            argv.iter()
                .filter(|a| a.starts_with("--"))
                .cloned()
                .collect::<Vec<_>>()
        };
        for tool in crate::flow::TOOLS {
            let info = adapter_info(tool).unwrap();
            let mut emitted: Vec<String> = Vec::new();
            // Only codex's fork arm consults this; every other tool ignores
            // it, and passing it unconditionally is what lets this loop stay
            // tool-agnostic instead of special-casing codex around itself.
            let codex_help = (tool == "codex").then_some(CODEX_HELP_WITH_FORK);
            for access in [Access::Read, Access::Write, Access::Full] {
                if let Ok(b) = build(tool, full_inp(access), &bp, Some("preassigned-id")) {
                    emitted.extend(long_flags(&b.argv));
                }
                if let Ok(b) = build_resume(tool, "resume-id", full_inp(access), &bp) {
                    emitted.extend(long_flags(&b.argv));
                }
                if let Ok(b) = build_fork(
                    tool,
                    "parent-id",
                    "child-id",
                    full_inp(access),
                    &bp,
                    codex_help,
                ) {
                    emitted.extend(long_flags(&b.argv));
                }
            }
            emitted.sort();
            emitted.dedup();
            for flag in &emitted {
                assert!(
                    info.required_flags.contains(&flag.as_str()),
                    "{tool} emits '{flag}' but required_flags does not list it - preflight's --help drift check would miss it disappearing"
                );
            }
        }
    }

    /// P1-06. The old rule ("every adapter's minimum_version is None") was a
    /// stand-in for a stricter one: never claim a version floor sfh did not
    /// verify. Agy's structured print output - the --output-format json
    /// envelope this preset's entire parse path depends on - is documented in
    /// agy's own changelog as shipping in 1.1.8, and LAST_VERIFIED here is
    /// 1.0.8: a floor below what the feature needs is not a floor at all, so
    /// 1.1.8 is pinned (with its source cited in the comment at the match
    /// arm). Every OTHER adapter still has no such documented floor, so
    /// `None` remains the honest answer for them - this test still fails if
    /// any of them starts claiming one without the same kind of evidence.
    ///
    /// NOTE: `src/preflight.rs`'s `no_adapter_claims_a_minimum_version_it_never_verified`
    /// asserts the OLD rule (every minimum is `None`) and will now fail on
    /// agy; that file is out of scope for this change (see the accompanying
    /// report) and needs the same "unless documented" rewrite this test gives
    /// the invariant here.
    #[test]
    fn only_agy_pins_a_minimum_version_and_it_names_its_source() {
        for tool in crate::flow::TOOLS {
            let i = adapter_info(tool).unwrap();
            if tool == "agy" {
                assert_eq!(
                    i.minimum_version,
                    Some("1.1.8"),
                    "agy's structured print output needs 1.1.8; pinning it stops sfh driving a build that cannot produce it"
                );
            } else {
                assert_eq!(
                    i.minimum_version, None,
                    "{tool} pins a floor that is not documented anywhere sfh can point to"
                );
            }
        }
    }
}
