//! A session-reporting stub CLI for sfh's behaviour tests (spec T-0, backlog B-15).
//!
//! `bin: "echo"` cannot report a session id, so every test that stands `echo`
//! in for claude trips F-11's "resume unverified" check and can only prove that
//! a guard did NOT fire - never that a session was actually continued. This
//! stub speaks the one shape sfh parses for those paths, `claude -p
//! --output-format json`: a single JSON envelope carrying .result, .session_id
//! and .usage. That makes resume and fork verifiable end to end without calling
//! an AI, on all three operating systems.
//!
//! Build (tests/engine_behaviour.sh does this once, into its own temp dir):
//!
//!   rustc -O --edition 2021 -o <dir>/sfh-session-stub tests/stub/session_stub.rs
//!
//! It lives in a SUBDIRECTORY of tests/ on purpose. Cargo auto-discovers
//! `tests/*.rs` (and `tests/*/main.rs`) as integration-test targets and compiles
//! them with --test, which would make this file's `main` dead code and fail
//! `cargo clippy --all-targets -- -D warnings`. Nothing here is a cargo target.
//!
//! # Session id
//!
//! The reported id is, in order: `--stub-session` / `SFH_STUB_SESSION`, the
//! value of `--session-id`, the value of `-r` / `--resume`, else a minted one.
//! Everything below the override is what real claude does, and is what the
//! tests depend on:
//!
//! - fresh  - sfh passes `--session-id <uuid>`            -> echo that
//! - resume - sfh passes `-r <uuid>`                      -> echo that
//! - fork   - sfh passes `-r <parent> --session-id <child>` -> echo the CHILD,
//!   because sfh fails the step when a fork reports the parent back.
//!
//! Every other argument sfh's presets add (`--permission-mode`, `--tools`, ...)
//! is ignored, and no unknown flag ever consumes the argument after it.
//!
//! # Control surface
//!
//! Each knob is a flag (`--x v` or `--x=v`) or an environment variable; the flag
//! wins. Unparseable values exit 2 rather than falling back to a default, so a
//! typo in a test cannot pass as a green run.
//!
//! | flag                | env                     | default    | meaning                                        |
//! |---------------------|-------------------------|------------|------------------------------------------------|
//! | --stub-last-line    | SFH_STUB_LAST_LINE      | STUB-OK    | final line of the body                          |
//! | --stub-quote        | SFH_STUB_QUOTE          | -          | emit this as a standalone line INSIDE the body  |
//! | --stub-exit         | SFH_STUB_EXIT           | 0          | process exit code                               |
//! | --stub-sleep        | SFH_STUB_SLEEP          | 0          | seconds to sleep before answering (accepts 0.5) |
//! | --stub-stderr-every | SFH_STUB_STDERR_EVERY_MS| -          | progress line on stderr every N ms while asleep |
//! | --stub-session      | SFH_STUB_SESSION        | see above  | force the reported session id                   |
//! | --stub-cost         | SFH_STUB_COST           | 0          | total_cost_usd                                  |
//! | --stub-tokens       | SFH_STUB_TOKENS         | 11,7       | usage input,output tokens                       |
//! | --stub-fail-once    | SFH_STUB_FAIL_ONCE      | -          | marker path; first invocation exits 1           |
//! | --stub-mkdir        | SFH_STUB_MKDIR           | -          | create a directory before returning              |
//! | --stub-plain        | SFH_STUB_PLAIN=1        | off        | print the body as plain text, no JSON envelope  |
//!
//! `--stub-last-line` and `--stub-quote` decode `\n`, `\r`, `\t` and `\\`, so a
//! test can produce a CRLF trailer or a multi-line verdict from one argument.
//!
//! `--stub-plain` exists for group members that need no session: it turns the
//! stub into a `cmd:` leaf that can still sleep, talk on stderr, and choose its
//! exit code - none of which `echo` or `printf` can do.
//!
//! `--version` and `--help` are answered first, before any option is parsed and
//! before any side effect, so a caller that only INSPECTS the binary (`sfh
//! preflight`, or the provenance probe every run makes) gets an answer without
//! the stub doing any of the work a real invocation would.
//!
//! # Protocols
//!
//! Since sfh 1.2 a preset step must complete its tool's documented machine
//! protocol - a terminal record has to arrive, and raw stdout is never promoted
//! to an answer - so `bin: "echo"` can no longer stand in for a preset at all.
//! The stub therefore speaks more than one shape and picks by what sfh's own
//! preset builder puts on the command line:
//!
//! | detected                      | protocol | terminal record            |
//! |-------------------------------|----------|----------------------------|
//! | `--output-last-message <path>`| codex    | `turn.completed` JSONL     |
//! | otherwise                     | claude   | `{"type":"result",...}`    |
//!
//! `--stub-protocol claude|codex` (or `SFH_STUB_PROTOCOL`) forces one, and
//! `--stub-no-terminal` withholds the terminal record so a test can prove sfh
//! fails closed on a truncated stream. In codex mode the body is written to the
//! `--output-last-message` file exactly as codex does, and the JSONL carries
//! the session as `thread.started.thread_id`.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

/// Always present, so a test that matches the last line proves the last line
/// was extracted rather than the whole body happening to be the verdict.
const PROSE: &str = "sfh-stub: prose, not a verdict";
/// Follows a `--stub-quote` line, so the quoted needle is never the last line.
const QUOTE_TAIL: &str = "sfh-stub: the line above was quoted, not decided";

#[derive(PartialEq, Clone, Copy)]
enum Protocol {
    Claude,
    Codex,
}

struct Config {
    session: String,
    last_line: String,
    quote: Option<String>,
    exit_code: i32,
    sleep: Duration,
    stderr_every: Option<Duration>,
    plain: bool,
    cost_usd: f64,
    input_tokens: u64,
    output_tokens: u64,
    mkdir: Option<String>,
    protocol: Protocol,
    /// Where codex is told to write its final answer.
    last_message_file: Option<String>,
    /// Withhold the documented terminal record, so a test can prove sfh refuses
    /// to call an unfinished protocol a success.
    no_terminal: bool,
}

fn die(msg: &str) -> ! {
    eprintln!("sfh-stub: {msg}");
    std::process::exit(2);
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// `\n`, `\r`, `\t`, `\\`; anything else keeps its backslash so a Windows path
/// passed by accident is not silently mangled.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Not a real UUID, and deliberately not shaped like one: nothing should be able
/// to confuse a minted stub id with an id sfh assigned.
fn minted_session() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("stub-session-{}-{:x}", std::process::id(), nanos)
}

fn parse_u64(what: &str, raw: &str) -> u64 {
    raw.trim()
        .parse::<u64>()
        .unwrap_or_else(|_| die(&format!("{what}: '{raw}' is not a non-negative integer")))
}

fn parse_config(argv: &[String]) -> Config {
    let mut session: Option<String> = None;
    let mut from_session_id: Option<String> = None;
    let mut from_resume: Option<String> = None;
    let mut last_line: Option<String> = None;
    let mut quote: Option<String> = None;
    let mut exit_code: Option<String> = None;
    let mut sleep: Option<String> = None;
    let mut stderr_every: Option<String> = None;
    let mut cost: Option<String> = None;
    let mut tokens: Option<String> = None;
    let mut fail_once: Option<String> = None;
    let mut mkdir: Option<String> = None;
    let mut plain = env_var("SFH_STUB_PLAIN").is_some();
    let mut protocol: Option<String> = None;
    let mut last_message_file: Option<String> = None;
    let mut no_terminal = env_var("SFH_STUB_NO_TERMINAL").is_some();

    let mut i = 0;
    while i < argv.len() {
        // Accept both `--flag value` (what sfh's presets emit) and `--flag=value`.
        let (name, inline) = match argv[i].split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_string(), Some(v.to_string())),
            _ => (argv[i].clone(), None),
        };
        let value = |i: &mut usize| -> String {
            if let Some(v) = inline.clone() {
                return v;
            }
            *i += 1;
            argv.get(*i)
                .cloned()
                .unwrap_or_else(|| die(&format!("{name} needs a value")))
        };
        match name.as_str() {
            "--session-id" => from_session_id = Some(value(&mut i)),
            "-r" | "--resume" => from_resume = Some(value(&mut i)),
            "--stub-session" => session = Some(value(&mut i)),
            "--stub-last-line" => last_line = Some(value(&mut i)),
            "--stub-quote" => quote = Some(value(&mut i)),
            "--stub-exit" => exit_code = Some(value(&mut i)),
            "--stub-sleep" => sleep = Some(value(&mut i)),
            "--stub-stderr-every" => stderr_every = Some(value(&mut i)),
            "--stub-cost" => cost = Some(value(&mut i)),
            "--stub-tokens" => tokens = Some(value(&mut i)),
            "--stub-fail-once" => fail_once = Some(value(&mut i)),
            "--stub-mkdir" => mkdir = Some(value(&mut i)),
            "--stub-plain" => plain = true,
            "--stub-protocol" => protocol = Some(value(&mut i)),
            "--stub-no-terminal" => no_terminal = true,
            // codex's own flag: sfh hands it a path and reads the answer back
            // from there, so the stub has to honour it to be codex-shaped.
            "--output-last-message" => last_message_file = Some(value(&mut i)),
            // Everything else belongs to the preset sfh is imitating. Skipping
            // it WITHOUT consuming the next argument is what keeps `--tools
            // Read,Grep` from eating a following `--session-id`.
            _ => {}
        }
        i += 1;
    }

    let session = session
        .or_else(|| env_var("SFH_STUB_SESSION"))
        .or(from_session_id)
        .or(from_resume)
        .unwrap_or_else(minted_session);
    let last_line = unescape(
        &last_line
            .or_else(|| env_var("SFH_STUB_LAST_LINE"))
            .unwrap_or_else(|| "STUB-OK".to_string()),
    );
    let quote = quote
        .or_else(|| env_var("SFH_STUB_QUOTE"))
        .map(|q| unescape(&q));
    let mut exit_code = match exit_code.or_else(|| env_var("SFH_STUB_EXIT")) {
        Some(raw) => raw
            .trim()
            .parse::<i32>()
            .unwrap_or_else(|_| die(&format!("--stub-exit: '{raw}' is not an integer"))),
        None => 0,
    };
    if let Some(marker) = fail_once.or_else(|| env_var("SFH_STUB_FAIL_ONCE")) {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(_) => exit_code = 1,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => die(&format!(
                "--stub-fail-once: cannot create marker '{marker}': {error}"
            )),
        }
    }
    let sleep = match sleep.or_else(|| env_var("SFH_STUB_SLEEP")) {
        Some(raw) => {
            let secs = raw
                .trim()
                .parse::<f64>()
                .unwrap_or_else(|_| die(&format!("--stub-sleep: '{raw}' is not a number")));
            if !secs.is_finite() || secs < 0.0 {
                die(&format!("--stub-sleep: '{raw}' must be finite and >= 0"));
            }
            Duration::from_secs_f64(secs)
        }
        None => Duration::ZERO,
    };
    let stderr_every = stderr_every
        .or_else(|| env_var("SFH_STUB_STDERR_EVERY_MS"))
        .map(|raw| Duration::from_millis(parse_u64("--stub-stderr-every", &raw).max(1)));
    let cost_usd = match cost.or_else(|| env_var("SFH_STUB_COST")) {
        Some(raw) => {
            let c = raw
                .trim()
                .parse::<f64>()
                .unwrap_or_else(|_| die(&format!("--stub-cost: '{raw}' is not a number")));
            // A non-finite cost would serialise to invalid JSON and sfh would
            // then report "the tool produced no envelope" - a misleading test.
            if !c.is_finite() {
                die(&format!("--stub-cost: '{raw}' must be finite"));
            }
            c
        }
        None => 0.0,
    };
    let (input_tokens, output_tokens) = match tokens.or_else(|| env_var("SFH_STUB_TOKENS")) {
        Some(raw) => match raw.split_once(',') {
            Some((a, b)) => (
                parse_u64("--stub-tokens input", a),
                parse_u64("--stub-tokens output", b),
            ),
            None => die(&format!("--stub-tokens: '{raw}' is not '<input>,<output>'")),
        },
        None => (11, 7),
    };
    let mkdir = mkdir.or_else(|| env_var("SFH_STUB_MKDIR"));
    let protocol = match protocol
        .or_else(|| env_var("SFH_STUB_PROTOCOL"))
        .as_deref()
    {
        Some("claude") => Protocol::Claude,
        Some("codex") => Protocol::Codex,
        Some(other) => die(&format!("--stub-protocol: unknown protocol '{other}'")),
        // sfh only passes --output-last-message when it is driving codex.
        None if last_message_file.is_some() => Protocol::Codex,
        None => Protocol::Claude,
    };

    Config {
        session,
        last_line,
        quote,
        exit_code,
        sleep,
        stderr_every,
        plain,
        cost_usd,
        input_tokens,
        output_tokens,
        mkdir,
        protocol,
        last_message_file,
        no_terminal,
    }
}

fn build_body(cfg: &Config) -> String {
    let mut body = String::new();
    body.push_str(PROSE);
    body.push('\n');
    if let Some(q) = &cfg.quote {
        body.push_str(q);
        body.push('\n');
        body.push_str(QUOTE_TAIL);
        body.push('\n');
    }
    body.push_str(&cfg.last_line);
    body
}

/// Sleeps for the configured time, optionally reporting on stderr as it goes.
/// One progress line is emitted even with no sleep, so the stderr mode is usable
/// on its own; each is flushed so the parent observes it while the child lives.
fn work(cfg: &Config) {
    let start = Instant::now();
    match cfg.stderr_every {
        Some(step) => {
            let mut n = 0u64;
            loop {
                n += 1;
                eprintln!("sfh-stub: progress {n}");
                let _ = std::io::stderr().flush();
                let elapsed = start.elapsed();
                if elapsed >= cfg.sleep {
                    break;
                }
                std::thread::sleep(step.min(cfg.sleep - elapsed));
            }
        }
        None => {
            if !cfg.sleep.is_zero() {
                std::thread::sleep(cfg.sleep);
            }
        }
    }
}

/// What a real CLI answers `--version` and `--help` with.
///
/// Answered BEFORE anything else, and before any of the stub's side effects, so
/// that a caller which only inspects the binary - `sfh preflight`, or the
/// provenance probe every run makes - is answered without the stub doing any of
/// the work a real invocation would. The flag list is the union of what sfh's
/// adapters look for, so this one binary can stand in for whichever preset a
/// test points it at.
const STUB_HELP: &str = "\
usage: sfh-session-stub [options]
  exec --json --output-last-message -s -c
  -p --output-format --permission-mode --session-id
  run --format --agent --auto
  --prompt-file
  --mode --print-timeout
  --offline --tools
  --trust --disable-project-configs
";

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // Before parse_config: --version and --help must not create the fail-once
    // marker, make a directory, or sleep. Inspecting a binary is not running it.
    if argv.iter().any(|a| a == "--version" || a == "-V") {
        println!("sfh-session-stub 1.0.0");
        return;
    }
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        print!("{STUB_HELP}");
        return;
    }
    let cfg = parse_config(&argv);

    // Drain the prompt the way a real CLI does. sfh closes the pipe after
    // writing, and gives cmd: steps a null stdin, so this always reaches EOF.
    let mut prompt = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut prompt);

    let start = Instant::now();
    work(&cfg);
    if let Some(path) = &cfg.mkdir {
        std::fs::create_dir(path)
            .unwrap_or_else(|error| die(&format!("--stub-mkdir: cannot create '{path}': {error}")));
    }
    let body = build_body(&cfg);

    let mut out = String::new();
    match (cfg.plain, cfg.protocol) {
        (true, _) => out.push_str(&body),
        (false, Protocol::Claude) => {
            // The shape sfh's ClaudeJson parser reads: one envelope on one line.
            // is_error stays false even for a non-zero --stub-exit, so a test can
            // ask for "a member that says the right thing and still fails" - the
            // exact case F1 exists to catch - without the two knobs interfering.
            // `type: result` is what makes it claude's TERMINAL record; without
            // it sfh 1.2 correctly refuses to read an answer out of the line.
            if cfg.no_terminal {
                out.push_str(r#"{"type":"system","subtype":"init","session_id":"#);
                json_string(&cfg.session, &mut out);
                out.push('}');
            } else {
                out.push_str(r#"{"type":"result","subtype":"success","is_error":false"#);
                out.push_str(",\"num_turns\":1,\"result\":");
                json_string(&body, &mut out);
                out.push_str(",\"session_id\":");
                json_string(&cfg.session, &mut out);
                out.push_str(&format!(
                    ",\"total_cost_usd\":{},\"duration_ms\":{}",
                    cfg.cost_usd,
                    start.elapsed().as_millis()
                ));
                out.push_str(&format!(
                    ",\"usage\":{{\"input_tokens\":{},\"output_tokens\":{}}}}}",
                    cfg.input_tokens, cfg.output_tokens
                ));
            }
        }
        (false, Protocol::Codex) => {
            // codex exec --json: a JSONL event log on stdout, with the final
            // answer written to --output-last-message. `turn.completed` is the
            // terminal record.
            if let Some(path) = &cfg.last_message_file {
                std::fs::write(path, &body).unwrap_or_else(|error| {
                    die(&format!(
                        "--output-last-message: cannot write '{path}': {error}"
                    ))
                });
            }
            out.push_str(r#"{"type":"thread.started","thread_id":"#);
            json_string(&cfg.session, &mut out);
            out.push_str("}\n");
            out.push_str(r#"{"type":"item.completed","item":{"type":"agent_message","text":"#);
            json_string(&body, &mut out);
            out.push_str("}}");
            if !cfg.no_terminal {
                out.push_str(&format!(
                    "\n{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":{},\"output_tokens\":{}}}}}",
                    cfg.input_tokens, cfg.output_tokens
                ));
            }
        }
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{out}");
    let _ = lock.flush();
    std::process::exit(cfg.exit_code);
}
