mod closure;
mod contain;
mod context;
mod doctor;
mod engine;
mod execute;
mod flow;
mod leaf;
mod machine;
mod preflight;
mod preset;
mod protocol;
mod runs;
mod sha256;
mod state;
mod template;
mod watch;
mod workspace;

use std::path::PathBuf;

const EXAMPLE: &str = include_str!("../examples/research.yaml");
const GUIDE: &str = include_str!("guide.txt");

const HELP: &str = "\
sfh - SimpleFlowHarness: chain AI CLI agents into staged flows

USAGE:
  sfh run <flow.yaml> [options]          Run a flow
  sfh status [run-dir] [--json]          Is a run still going? (default: newest run)
  sfh wait [run-dir] [--timeout SEC]     Block until a run finishes, then print its result
  sfh stop [run-dir]                     Cancel a run and its agents
  sfh doctor [flow.yaml]                 Check the presets still match the installed CLIs
  sfh validate <flow.yaml> [options]      Parse and static-check a flow file
  sfh plan <flow.yaml> [--var k=v]        Resolve commands without executing
  sfh graph <flow.yaml> [--mermaid]       Show control-flow edges
  sfh config show <flow.yaml>             Show merged config (env values redacted)
  sfh init [file] [--force]              Write an example flow file (default: flow.yaml)
  sfh guide                              Show the compact AI-oriented flow guide
  sfh preflight [flow.yaml] [--json] [--probe-binaries]
                                          Local capability check, no model calls (--probe-binaries
                                          also RUNS a bin: override's --help/--version, not just claude/codex/etc.'s own)
  sfh help [command]                     Show overall or command-specific usage
  sfh runs list|show|why|clean [...]     Browse, explain or prune past runs
  sfh workspaces list|show|clean|remove  Inspect or prune managed workspaces

RUN OPTIONS:
  --var key=value     Override a flow variable (repeatable)
  --emit <step-id>    Print this step's output at the end (default: last executed step)
  --runs-dir <dir>    Where to store run artifacts (default: .sfh/runs)
  --run-dir <dir>     Use this exact run directory (advanced; choose a new/empty path)
  --detach            Run in the background, print the run dir, and exit at once.
                      The run survives this shell and its parent; poll it with
                      `sfh status` and collect it with `sfh wait`.
  --state-dir <dir>   Root for runs/workspaces/plans/doctor (or SFH_STATE_DIR).
                      Required for a managed workspace unless the platform
                      user-state dir can be determined. --runs-dir still wins
                      for run artifacts, and without either, runs stay in
                      .sfh/runs.
  --profiles <file>   Profile overlay file (repeatable; later files win)
  --json              Answer with a machine envelope. stdout carries JSON and
                      nothing else; progress goes to stderr.
  --resume <run-dir>  Continue a previous run, reusing its finished steps
  --resume-latest     Same, picking the newest run dir
  --force-resume      Resume even though the flow file or the execution closure
                      changed (profile overlays, context files, tool versions)
  --adopt-workspace   Resume even though the managed workspace changed, taking
                      its current contents as the new baseline. A DIFFERENT
                      question from --force-resume; neither implies the other.
  --carry-budget-from <run-dir>
                      Start a FRESH run that inherits that run's spend: step
                      runs, visits, cost and active time. For when the flow had
                      to be FIXED, so --resume rightly refuses, and the budget
                      already spent would otherwise reset to zero.
  --no-partial-emit   On failure, do not print the best available output
  --dry-run           Render prompts/commands without executing anything
  -v, --verbose       Print full command lines
  -q, --quiet         Suppress progress output (stdout gets the result only)

STATUS / WAIT / STOP OPTIONS:
  status [run-dir] [--runs-dir d] [--json]
  wait   [run-dir] [--runs-dir d] [--timeout SEC] [--interval SEC] [-q]
  stop   [run-dir] [--runs-dir d]
  status exit codes: 0 = done, 1 = failed/dead/stopped, 2 = cannot tell,
                     3 = running, 4 = stuck (a step routed to `goto: stuck`)
  wait exits with the flow's own code (0/1/4), or 3 if --timeout elapsed first
  (a wait timeout does NOT cancel the run - use `sfh stop` for that)

PREFLIGHT OPTIONS:
  preflight [flow.yaml] [--profiles file] [--state-dir d] [--json] [--probe-binaries]
  Free and offline: which binaries are installed, at what version, whether the
  flags each adapter depends on are still in their --help, which protocol they
  speak, what access it can actually enforce, and what workspace and context
  this flow would build. Makes NO model calls - `sfh doctor` is that check.
  A tool's own default launcher (e.g. claude, codex) is always probed: sfh
  ships that adapter and has verified its --help/--version are inert. A
  bin: override names an arbitrary program the flow chose, so by default it
  is only RESOLVED, never run - the same rule a cmd: step's program already
  gets. Pass --probe-binaries to actually EXECUTE a bin: override's own
  --help/--version too (needs a flow file; a flowless survey has none).

DOCTOR OPTIONS:
  doctor [flow.yaml] [--runs-dir d] [--state-dir d] [--timeout SEC]
  Sends a one-token prompt to each tool and checks sfh can still parse the
  answer, so preset drift surfaces here instead of halfway through a paid run.
  This makes REAL calls. With a flow file it checks exactly that flow's tools
  (honouring bin:/model: from its profiles) and a missing tool is an error;
  without one it probes every preset and just reports which are absent.

RUNS OPTIONS:
  runs list [--runs-dir d] [-n N] [--json]
  runs show <run-dir> [--json]
  runs why  <run-dir> [--json]
  runs clean [--runs-dir d] [--older-than DAYS] [--keep N] [--dry-run]

WORKSPACE OPTIONS:
  workspaces list [--runs-dir d] [--state-dir d] [--json]
  workspaces show <run-dir> [--json]
  workspaces clean [--older-than DAYS] [--dry-run] [--json]
  workspaces remove <run-dir> [--discard] [--json]
  sfh removes only a workspace it created (ownership marker + run nonce), and
  never discards uncommitted work without --discard.

EXIT CODES:
  0 = flow succeeded    1 = flow failed    2 = config/usage error
  4 = flow stuck (a step routed to `goto: stuck`: work saved, needs a human)

FLOW FILE (see `sfh init` for a full example, schema/flow.schema.json for the schema):
  Steps run top-to-bottom unless a route: rule redirects. Templates:
  {{vars.name}} {{steps.<id>.output}} {{steps.<id>.outputs}} {{steps.<id>.output_file}}
  {{steps.<id>.exit}} {{steps.<id>.stderr_file}}
  {{item}} {{item_index}} {{notes}} {{run_dir}} {{flow_dir}} {{prompt_file}}
  {{step_id}} {{visit}} {{os}}
  {{budget.spent_usd}} {{budget.elapsed_sec}} {{budget.remaining_usd}} {{budget.remaining_sec}}
  (remaining_* is the string `unlimited` when that axis has no ceiling)
  {{context}} {{context_file}} (when the step names a context)
  Filters: | head:N | tail:N | truncate:N | lines:A-B | trim | optional | default:text
  Preset tools: codex, claude, opencode, grok, agy, pi, cursor.
  Custom cmd: array form = spawned directly; string form = via cmd /C | sh -c.
  Opt-in since v1.2 (omit them all and nothing changes):
    workspace:  mode: current|directory|git-worktree|auto - where side effects go
    contexts:   named file/inline/template sources, hashed and recorded
    effects:    read|workspace|external|unknown, per step
    replay:     unfinished: rerun|stuck|fail - what a crash-resume may re-run
";

fn main() {
    execute::install_process_guard();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("help") => match args.as_slice() {
            [_] => {
                print!("{HELP}");
                0
            }
            [_, command] => cmd_help(command),
            _ => usage_err("usage: sfh help [command]"),
        },
        // These have a second command word, so let their own parser choose the
        // precise nested help instead of collapsing e.g. `runs list --help`
        // into the generic `runs` usage.
        Some("config")
            if args
                .iter()
                .skip(1)
                .any(|arg| arg == "-h" || arg == "--help") =>
        {
            cmd_config(&args[1..])
        }
        Some("runs")
            if args
                .iter()
                .skip(1)
                .any(|arg| arg == "-h" || arg == "--help") =>
        {
            cmd_runs(&args[1..])
        }
        Some("workspaces")
            if args
                .iter()
                .skip(1)
                .any(|arg| arg == "-h" || arg == "--help") =>
        {
            cmd_workspaces(&args[1..])
        }
        Some(cmd)
            if args
                .iter()
                .skip(1)
                .any(|arg| arg == "-h" || arg == "--help") =>
        {
            cmd_help(cmd)
        }
        Some("run") => cmd_run(&args[1..]),
        Some("plan") => cmd_plan(&args[1..]),
        Some("preflight") => cmd_preflight(&args[1..]),
        Some("workspaces") => cmd_workspaces(&args[1..]),
        Some("graph") => cmd_graph(&args[1..]),
        Some("config") => cmd_config(&args[1..]),
        Some("status") => cmd_watch(&args[1..], Watch::Status),
        Some("wait") => cmd_watch(&args[1..], Watch::Wait),
        Some("stop") => cmd_watch(&args[1..], Watch::Stop),
        Some("doctor") => cmd_doctor(&args[1..]),
        Some("validate") => cmd_validate(&args[1..]),
        Some("init") => cmd_init(&args[1..]),
        Some("guide") => cmd_guide(&args[1..]),
        Some("runs") => cmd_runs(&args[1..]),
        Some("-V") | Some("--version") | Some("version") => {
            println!("sfh {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("-h") | Some("--help") | None => {
            print!("{HELP}");
            0
        }
        Some(other) => {
            eprintln!("sfh: unknown command '{other}'\n");
            eprint!("{HELP}");
            2
        }
    };
    std::process::exit(code);
}

fn cmd_help(command: &str) -> i32 {
    let usage = match command {
        "run" => "sfh run <flow.yaml> [--var k=v] [--emit id] [--runs-dir d] [--state-dir d] [--profiles file] [--run-dir d] [--detach] [--json] [--resume dir|--resume-latest] [--force-resume] [--adopt-workspace] [--carry-budget-from dir] [--no-partial-emit] [--dry-run] [-v|-q]\nCommand steps can read their fully rendered prompt from {{prompt_file}}.\n--force-resume waives the flow/closure check; --adopt-workspace waives the workspace check. They are separate.\n--carry-budget-from starts a NEW run holding an earlier run's spend, for when the flow itself had to be fixed.",
        "plan" => "sfh plan <flow.yaml> [--var k=v] [--profiles file] [--state-dir d] [--save [dir]] [--json]\nResolves every command in an isolated temporary directory and executes nothing.\n--save keeps the rendered prompts, context bundles and machine plan for review.",
        "graph" => "sfh graph <flow.yaml> [--mermaid]",
        "config" => "sfh config show <flow.yaml> [--profiles file] [--show-secrets]",
        "validate" => "sfh validate <flow.yaml> [--var k=v] [--profiles file] [--strict] [--json]",
        "status" => "sfh status [run-dir] [--runs-dir d] [--state-dir d] [--json]",
        "wait" => "sfh wait [run-dir] [--runs-dir d] [--state-dir d] [--timeout SEC] [--interval SEC] [-q] [--json]",
        "stop" => "sfh stop [run-dir] [--runs-dir d] [--state-dir d] [--json]",
        "doctor" => "sfh doctor [flow.yaml] [--runs-dir d] [--state-dir d] [--timeout SEC]\nMakes REAL model calls, from an isolated scratch directory. `sfh preflight` is the free check.",
        "init" => "sfh init [file] [--force]",
        "guide" => "sfh guide",
        "runs" => "sfh runs list|show|why|clean [options]",
        "preflight" => "sfh preflight [flow.yaml] [--profiles file] [--state-dir d] [--json] [--probe-binaries]\nLocal capability check. Makes NO model calls - `sfh doctor` does that.\nA bin: override is resolved but never run unless --probe-binaries is given, which DOES execute its --help/--version.",
        "workspaces" => "sfh workspaces list|show|clean|remove [options]",
        _ => {
            print!("{HELP}");
            return 2;
        }
    };
    println!("{usage}");
    0
}

fn usage_err(msg: &str) -> i32 {
    eprintln!("sfh: {msg}");
    2
}

fn need<'a>(rest: &'a [String], i: &mut usize, what: &str) -> Result<&'a String, String> {
    *i += 1;
    rest.get(*i).ok_or_else(|| format!("{what} needs a value"))
}

fn parse_vars_flag(
    rest: &[String],
    i: &mut usize,
    out: &mut Vec<(String, String)>,
) -> Result<(), String> {
    let kv = need(rest, i, "--var")?;
    let (k, v) = kv
        .split_once('=')
        .ok_or_else(|| format!("--var needs key=value, got '{kv}'"))?;
    out.push((k.to_string(), v.to_string()));
    Ok(())
}

fn cmd_run(rest: &[String]) -> i32 {
    let mut flow_path: Option<PathBuf> = None;
    let mut opts = engine::RunOpts::default();
    let mut i = 0;
    while i < rest.len() {
        let r = match rest[i].as_str() {
            "--var" => parse_vars_flag(rest, &mut i, &mut opts.vars),
            "--emit" => need(rest, &mut i, "--emit").map(|v| opts.emit = Some(v.clone())),
            "--runs-dir" => {
                need(rest, &mut i, "--runs-dir").map(|v| opts.runs_dir = Some(PathBuf::from(v)))
            }
            "--state-dir" => {
                need(rest, &mut i, "--state-dir").map(|v| opts.state_dir = Some(PathBuf::from(v)))
            }
            "--profiles" => {
                need(rest, &mut i, "--profiles").map(|v| opts.profiles.push(PathBuf::from(v)))
            }
            "--json" => {
                opts.as_json = true;
                Ok(())
            }
            // Optional value: `--save` alone puts the plan under the state
            // root, `--save <dir>` puts it exactly where the caller says.
            "--save" => {
                let next = rest.get(i + 1);
                let dir = match next {
                    Some(v) if !v.starts_with('-') => {
                        i += 1;
                        Some(PathBuf::from(v))
                    }
                    _ => None,
                };
                opts.save_plan = Some(dir);
                Ok(())
            }
            "--adopt-workspace" => {
                opts.adopt_workspace = true;
                Ok(())
            }
            "--carry-budget-from" => need(rest, &mut i, "--carry-budget-from")
                .map(|v| opts.carry_budget_from = Some(PathBuf::from(v))),
            "--resume" => {
                need(rest, &mut i, "--resume").map(|v| opts.resume = Some(PathBuf::from(v)))
            }
            "--resume-latest" => {
                opts.resume_latest = true;
                Ok(())
            }
            "--force-resume" => {
                opts.force_resume = true;
                Ok(())
            }
            "--no-partial-emit" => {
                opts.no_partial_emit = true;
                Ok(())
            }
            "--detach" => {
                opts.detach = true;
                Ok(())
            }
            // Set by --detach when it hands off to the background copy; also
            // usable directly to pin a run to a known directory.
            "--run-dir" => {
                need(rest, &mut i, "--run-dir").map(|v| opts.run_dir = Some(PathBuf::from(v)))
            }
            "--dry-run" => {
                opts.dry_run = true;
                Ok(())
            }
            "-v" | "--verbose" => {
                opts.verbose = true;
                Ok(())
            }
            "-q" | "--quiet" => {
                opts.quiet = true;
                Ok(())
            }
            s if s.starts_with('-') => Err(format!("unknown flag '{s}'")),
            s => {
                if flow_path.is_some() {
                    Err("more than one flow file given".to_string())
                } else {
                    flow_path = Some(PathBuf::from(s));
                    Ok(())
                }
            }
        };
        if let Err(e) = r {
            return usage_err(&e);
        }
        i += 1;
    }
    let Some(fp) = flow_path else {
        return usage_err("usage: sfh run <flow.yaml> [--var k=v]... [--emit id] [--resume dir] [--dry-run] [-v] [-q]");
    };
    opts.flow_path = fp;
    if opts.resume.is_some() && opts.resume_latest {
        return usage_err("--resume and --resume-latest are mutually exclusive");
    }
    if opts.force_resume && opts.resume.is_none() && !opts.resume_latest {
        return usage_err("--force-resume requires --resume or --resume-latest");
    }
    if opts.adopt_workspace && opts.resume.is_none() && !opts.resume_latest {
        return usage_err("--adopt-workspace requires --resume or --resume-latest");
    }
    if opts.carry_budget_from.is_some() && (opts.resume.is_some() || opts.resume_latest) {
        return usage_err(
            "--carry-budget-from and --resume are different answers: resume continues the earlier run when the flow is unchanged, carry starts a new run when you had to fix the flow",
        );
    }
    if opts.verbose && opts.quiet {
        return usage_err("--verbose and --quiet are mutually exclusive");
    }
    if opts.as_json && opts.verbose {
        return usage_err("--json and --verbose are mutually exclusive (JSON mode keeps stdout to the envelope alone)");
    }
    if opts.detach && opts.dry_run {
        return usage_err(
            "--detach and --dry-run do nothing together (a dry run has nothing to detach)",
        );
    }
    engine::run(opts)
}

fn cmd_plan(rest: &[String]) -> i32 {
    let mut flow_files = 0usize;
    let mut i = 0usize;
    while i < rest.len() {
        match rest[i].as_str() {
            // Options plan accepts and passes through to the shared run path.
            // Everything else run-only is still rejected, so `plan --detach`
            // cannot quietly become something plan does not mean.
            "--var" | "--profiles" | "--state-dir" | "--runs-dir" => {
                if i + 1 >= rest.len() {
                    return usage_err(&format!("{} needs a value", rest[i]));
                }
                i += 1;
            }
            "--json" => {}
            "--save" => {
                if rest.get(i + 1).is_some_and(|v| !v.starts_with('-')) {
                    i += 1;
                }
            }
            flag if flag.starts_with('-') => return usage_err(&format!("unknown flag '{flag}'")),
            _ => {
                flow_files += 1;
                if flow_files > 1 {
                    return usage_err("more than one flow file given");
                }
            }
        }
        i += 1;
    }
    let mut args = rest.to_vec();
    args.push("--dry-run".into());
    cmd_run(&args)
}

fn cmd_preflight(rest: &[String]) -> i32 {
    let mut flow_path: Option<PathBuf> = None;
    let mut state_dir: Option<PathBuf> = None;
    let mut profiles: Vec<PathBuf> = Vec::new();
    let mut as_json = false;
    let mut probe_binaries = false;
    let mut i = 0;
    while i < rest.len() {
        let r: Result<(), String> = match rest[i].as_str() {
            "--state-dir" => {
                need(rest, &mut i, "--state-dir").map(|v| state_dir = Some(PathBuf::from(v)))
            }
            "--profiles" => {
                need(rest, &mut i, "--profiles").map(|v| profiles.push(PathBuf::from(v)))
            }
            "--json" => {
                as_json = true;
                Ok(())
            }
            // P0-05: a bin: override is resolved but never run by default -
            // this is the opt-in to actually execute its --help/--version
            // too, same as a tool sfh ships support for already gets.
            "--probe-binaries" => {
                probe_binaries = true;
                Ok(())
            }
            s if s.starts_with('-') => Err(format!("unknown flag '{s}'")),
            s => {
                if flow_path.is_some() {
                    Err("more than one flow file given".into())
                } else {
                    flow_path = Some(PathBuf::from(s));
                    Ok(())
                }
            }
        };
        if let Err(e) = r {
            return usage_err(&e);
        }
        i += 1;
    }
    if !profiles.is_empty() && flow_path.is_none() {
        return usage_err("--profiles needs a flow file to apply the overlay to");
    }
    if probe_binaries && flow_path.is_none() {
        return usage_err(
            "--probe-binaries needs a flow file: a flowless survey only ever probes each adapter's own default launcher, which is already probed without it",
        );
    }
    let root = state::StateRoot::resolve(state_dir.as_deref(), None);
    preflight::run(
        flow_path.as_deref(),
        &root,
        &profiles,
        as_json,
        probe_binaries,
    )
}

fn cmd_workspaces(rest: &[String]) -> i32 {
    let sub = rest.first().map(String::as_str).unwrap_or("list");
    if rest.iter().any(|arg| arg == "-h" || arg == "--help") {
        let usage = match sub {
            "list" => "sfh workspaces list [--runs-dir d] [--state-dir d] [--json]",
            "show" => "sfh workspaces show <run-dir> [--json]",
            "clean" => {
                "sfh workspaces clean [--runs-dir d] [--older-than DAYS] [--dry-run] [--json]"
            }
            "remove" => "sfh workspaces remove <run-dir> [--discard] [--json]",
            _ => "sfh workspaces list|show|clean|remove [options]",
        };
        println!("{usage}");
        return if matches!(sub, "list" | "show" | "clean" | "remove" | "-h" | "--help") {
            0
        } else {
            2
        };
    }
    let mut runs_dir: Option<PathBuf> = None;
    let mut state_dir: Option<PathBuf> = None;
    let mut target: Option<PathBuf> = None;
    let mut as_json = false;
    let mut dry_run = false;
    let mut discard = false;
    let mut days: Option<u64> = None;
    let mut i = 1;
    while i < rest.len() {
        let r: Result<(), String> = match rest[i].as_str() {
            "--runs-dir" => {
                need(rest, &mut i, "--runs-dir").map(|v| runs_dir = Some(PathBuf::from(v)))
            }
            "--state-dir" => {
                need(rest, &mut i, "--state-dir").map(|v| state_dir = Some(PathBuf::from(v)))
            }
            "--json" => {
                as_json = true;
                Ok(())
            }
            "--dry-run" if sub == "clean" => {
                dry_run = true;
                Ok(())
            }
            // The only way an sfh command discards uncommitted work, and a
            // human has to type it.
            "--discard" if sub == "remove" => {
                discard = true;
                Ok(())
            }
            "--older-than" if sub == "clean" => need(rest, &mut i, "--older-than")
                .and_then(|v| {
                    v.strip_suffix('d')
                        .unwrap_or(v)
                        .parse()
                        .map_err(|_| "--older-than needs days".to_string())
                })
                .map(|v| days = Some(v)),
            s if s.starts_with('-') => Err(format!("unknown flag '{s}'")),
            s if sub == "show" || sub == "remove" => {
                if target.is_some() {
                    Err("more than one run dir given".into())
                } else {
                    target = Some(PathBuf::from(s));
                    Ok(())
                }
            }
            s => Err(format!(
                "workspaces {sub} does not accept positional argument '{s}'"
            )),
        };
        if let Err(e) = r {
            return usage_err(&e);
        }
        i += 1;
    }
    let root = state::StateRoot::resolve(state_dir.as_deref(), runs_dir.as_deref());
    match sub {
        "list" => runs::workspaces_list(&root.runs_dir(), as_json),
        "show" => match target {
            Some(d) => runs::workspaces_show(&d, as_json),
            None => usage_err("usage: sfh workspaces show <run-dir>"),
        },
        "clean" => runs::workspaces_clean(&root.runs_dir(), days, dry_run, as_json),
        "remove" => match target {
            Some(d) => runs::workspaces_remove(&d, discard, as_json),
            None => usage_err("usage: sfh workspaces remove <run-dir> [--discard]"),
        },
        other => usage_err(&format!(
            "unknown workspaces subcommand '{other}' (list/show/clean/remove)"
        )),
    }
}

fn cmd_graph(rest: &[String]) -> i32 {
    let mut path = None;
    let mut mermaid = false;
    for arg in rest {
        match arg.as_str() {
            "--mermaid" => mermaid = true,
            flag if flag.starts_with('-') => return usage_err(&format!("unknown flag '{flag}'")),
            value if path.is_some() => {
                return usage_err(&format!("more than one flow file given (extra: '{value}')"))
            }
            value => path = Some(PathBuf::from(value)),
        }
    }
    match path {
        Some(path) => engine::graph(&path, mermaid),
        None => usage_err("usage: sfh graph <flow.yaml> [--mermaid]"),
    }
}

fn cmd_config(rest: &[String]) -> i32 {
    if rest.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("sfh config show <flow.yaml> [--profiles file] [--show-secrets]");
        return 0;
    }
    if rest.first().map(String::as_str) != Some("show") {
        return usage_err("usage: sfh config show <flow.yaml> [--show-secrets]");
    }
    let mut path: Option<PathBuf> = None;
    let mut show_secrets = false;
    let mut profiles: Vec<PathBuf> = Vec::new();
    let mut i = 1;
    while i < rest.len() {
        match rest[i].as_str() {
            "--show-secrets" => show_secrets = true,
            "--profiles" => match need(rest, &mut i, "--profiles") {
                Ok(v) => profiles.push(PathBuf::from(v)),
                Err(e) => return usage_err(&e),
            },
            flag if flag.starts_with('-') => return usage_err(&format!("unknown flag '{flag}'")),
            value if path.is_some() => {
                return usage_err(&format!("more than one flow file given (extra: '{value}')"))
            }
            value => path = Some(PathBuf::from(value)),
        }
        i += 1;
    }
    let Some(path) = path else {
        return usage_err("usage: sfh config show <flow.yaml> [--show-secrets]");
    };
    if show_secrets {
        eprintln!(
            "sfh: warning: --show-secrets prints environment values; treat stdout as sensitive"
        );
    }
    engine::show_config(&path, show_secrets, &profiles)
}

#[derive(PartialEq, Clone, Copy)]
enum Watch {
    Status,
    Wait,
    Stop,
}

fn cmd_watch(rest: &[String], mode: Watch) -> i32 {
    let mut runs_dir: Option<PathBuf> = None;
    let mut state_dir: Option<PathBuf> = None;
    let mut target: Option<PathBuf> = None;
    let mut as_json = false;
    let mut quiet = false;
    let mut timeout: Option<u64> = None;
    let mut interval = 3u64;
    let is_wait = mode == Watch::Wait;
    let mut i = 0;
    while i < rest.len() {
        let r: Result<(), String> = match rest[i].as_str() {
            "--runs-dir" => {
                need(rest, &mut i, "--runs-dir").map(|v| runs_dir = Some(PathBuf::from(v)))
            }
            "--state-dir" => {
                need(rest, &mut i, "--state-dir").map(|v| state_dir = Some(PathBuf::from(v)))
            }
            "--json" => {
                as_json = true;
                Ok(())
            }
            "--timeout" if is_wait => need(rest, &mut i, "--timeout")
                .and_then(|v| v.parse().map_err(|_| "--timeout needs seconds".to_string()))
                .map(|v| timeout = Some(v)),
            "--interval" if is_wait => need(rest, &mut i, "--interval")
                .and_then(|v| {
                    v.parse()
                        .map_err(|_| "--interval needs seconds".to_string())
                })
                .map(|v| interval = v),
            "-q" | "--quiet" if is_wait => {
                quiet = true;
                Ok(())
            }
            s if s.starts_with('-') => Err(format!("unknown flag '{s}'")),
            s => {
                if target.is_some() {
                    Err("more than one run dir given".to_string())
                } else {
                    target = Some(PathBuf::from(s));
                    Ok(())
                }
            }
        };
        if let Err(e) = r {
            return usage_err(&e);
        }
        i += 1;
    }
    if as_json && quiet {
        return usage_err("--json and --quiet are mutually exclusive (JSON mode already keeps stdout to the envelope alone)");
    }
    let runs_dir = state::StateRoot::resolve(state_dir.as_deref(), runs_dir.as_deref()).runs_dir();
    match mode {
        Watch::Wait => watch::wait(
            target.as_deref(),
            &runs_dir,
            timeout,
            interval,
            quiet || as_json,
            as_json,
        ),
        Watch::Stop => watch::stop(target.as_deref(), &runs_dir, as_json),
        Watch::Status => watch::status(target.as_deref(), &runs_dir, as_json),
    }
}

fn cmd_doctor(rest: &[String]) -> i32 {
    let mut flow_path: Option<PathBuf> = None;
    let mut runs_dir: Option<PathBuf> = None;
    let mut state_dir: Option<PathBuf> = None;
    let mut timeout = 120u64;
    let mut i = 0;
    while i < rest.len() {
        let r: Result<(), String> = match rest[i].as_str() {
            "--runs-dir" => {
                need(rest, &mut i, "--runs-dir").map(|v| runs_dir = Some(PathBuf::from(v)))
            }
            "--state-dir" => {
                need(rest, &mut i, "--state-dir").map(|v| state_dir = Some(PathBuf::from(v)))
            }
            "--timeout" => need(rest, &mut i, "--timeout")
                .and_then(|v| v.parse().map_err(|_| "--timeout needs seconds".to_string()))
                .map(|v| timeout = v),
            s if s.starts_with('-') => Err(format!("unknown flag '{s}'")),
            s => {
                if flow_path.is_some() {
                    Err("more than one flow file given".to_string())
                } else {
                    flow_path = Some(PathBuf::from(s));
                    Ok(())
                }
            }
        };
        if let Err(e) = r {
            return usage_err(&e);
        }
        i += 1;
    }
    let root = state::StateRoot::resolve(state_dir.as_deref(), runs_dir.as_deref());
    let work = doctor::default_work_dir(&root.runs_dir(), &root);
    doctor::run(flow_path.as_deref(), timeout, &work)
}

fn cmd_validate(rest: &[String]) -> i32 {
    let mut flow_path: Option<PathBuf> = None;
    let mut vars: Vec<(String, String)> = Vec::new();
    let mut strict = false;
    let mut as_json = false;
    let mut profiles: Vec<PathBuf> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--var" => {
                if let Err(e) = parse_vars_flag(rest, &mut i, &mut vars) {
                    return usage_err(&e);
                }
            }
            "--profiles" => match need(rest, &mut i, "--profiles") {
                Ok(v) => profiles.push(PathBuf::from(v)),
                Err(e) => return usage_err(&e),
            },
            "--strict" => strict = true,
            "--json" => as_json = true,
            s if s.starts_with('-') => return usage_err(&format!("unknown flag '{s}'")),
            s => {
                if flow_path.is_some() {
                    return usage_err("more than one flow file given");
                }
                flow_path = Some(PathBuf::from(s));
            }
        }
        i += 1;
    }
    let Some(fp) = flow_path else {
        return usage_err("usage: sfh validate <flow.yaml> [--var k=v]...");
    };
    if strict || as_json || !profiles.is_empty() {
        engine::validate_with_options(&fp, &vars, strict, as_json, &profiles)
    } else {
        engine::validate(&fp, &vars)
    }
}

fn cmd_init(rest: &[String]) -> i32 {
    let mut path = PathBuf::from("flow.yaml");
    let mut path_given = false;
    let mut force = false;
    for a in rest {
        match a.as_str() {
            "--force" => force = true,
            s if s.starts_with('-') => return usage_err(&format!("unknown flag '{s}'")),
            s if path_given => {
                return usage_err(&format!("more than one output file given (extra: '{s}')"))
            }
            s => {
                path = PathBuf::from(s);
                path_given = true;
            }
        }
    }
    if path.exists() && !force {
        return usage_err(&format!(
            "{} already exists (use --force to overwrite)",
            path.display()
        ));
    }
    match std::fs::write(&path, EXAMPLE) {
        Ok(()) => {
            eprintln!("wrote {}", path.display());
            eprintln!(
                "next: edit it, then `sfh validate {0}` and `sfh run {0}`",
                execute::shell_quote(&path.display().to_string())
            );
            0
        }
        Err(e) => usage_err(&format!("cannot write {}: {e}", path.display())),
    }
}

fn cmd_guide(rest: &[String]) -> i32 {
    if !rest.is_empty() {
        return usage_err("usage: sfh guide");
    }
    print!("{GUIDE}");
    0
}

fn cmd_runs(rest: &[String]) -> i32 {
    let mut runs_dir = PathBuf::from(".sfh").join("runs");
    let mut limit = 20usize;
    let mut days = 30u64;
    let mut keep = 5usize;
    let mut dry = false;
    let mut as_json = false;
    let mut target: Option<PathBuf> = None;
    let sub = rest.first().map(String::as_str).unwrap_or("list");
    if rest.iter().any(|arg| arg == "-h" || arg == "--help") {
        let usage = match sub {
            "list" => "sfh runs list [--runs-dir d] [-n N] [--json]",
            "show" => "sfh runs show <run-dir> [--json]",
            "why" => "sfh runs why <run-dir> [--json]",
            "clean" => "sfh runs clean [--runs-dir d] [--older-than DAYS] [--keep N] [--dry-run]",
            "-h" | "--help" => "sfh runs list|show|why|clean [options]",
            _ => "sfh runs list|show|why|clean [options]",
        };
        println!("{usage}");
        return if matches!(sub, "list" | "show" | "why" | "clean" | "-h" | "--help") {
            0
        } else {
            2
        };
    }
    let mut i = 1;
    while i < rest.len() {
        let r: Result<(), String> = match rest[i].as_str() {
            "--runs-dir" if sub == "list" || sub == "clean" => {
                need(rest, &mut i, "--runs-dir").map(|v| runs_dir = PathBuf::from(v))
            }
            flag @ ("-n" | "--limit") if sub == "list" => {
                let label = flag.to_string();
                need(rest, &mut i, &label)
                    .and_then(|v| v.parse().map_err(|_| format!("{label} needs a number")))
                    .map(|v| limit = v)
            }
            "--older-than" if sub == "clean" => need(rest, &mut i, "--older-than")
                .and_then(|v| {
                    v.strip_suffix('d')
                        .unwrap_or(v)
                        .parse()
                        .map_err(|_| "--older-than needs days".to_string())
                })
                .map(|v| days = v),
            "--keep" if sub == "clean" => need(rest, &mut i, "--keep")
                .and_then(|v| v.parse().map_err(|_| "--keep needs a number".to_string()))
                .map(|v| keep = v),
            "--dry-run" if sub == "clean" => {
                dry = true;
                Ok(())
            }
            "--json" if sub == "list" || sub == "show" || sub == "why" => {
                as_json = true;
                Ok(())
            }
            s if s.starts_with('-') => Err(format!("unknown flag '{s}'")),
            s if sub == "show" || sub == "why" => {
                if target.is_some() {
                    Err("more than one run dir given".into())
                } else {
                    target = Some(PathBuf::from(s));
                    Ok(())
                }
            }
            s => Err(format!(
                "runs {sub} does not accept positional argument '{s}'"
            )),
        };
        if let Err(e) = r {
            return usage_err(&e);
        }
        i += 1;
    }
    match sub {
        "list" => runs::list(&runs_dir, limit, as_json),
        "show" => match target {
            Some(d) => runs::show(&d, as_json),
            None => usage_err("usage: sfh runs show <run-dir>"),
        },
        "why" => match target {
            Some(d) => runs::why(&d, as_json),
            None => usage_err("usage: sfh runs why <run-dir> [--json]"),
        },
        "clean" => runs::clean(&runs_dir, days, keep, dry),
        other => usage_err(&format!(
            "unknown runs subcommand '{other}' (list/show/why/clean)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{cmd_config, cmd_init, cmd_plan, cmd_runs, cmd_watch, Watch, GUIDE};

    fn terminal_run(root: &std::path::Path, name: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let pid = std::process::id();
        let start = crate::execute::pid_start_time(pid);
        crate::contain::write_nonce(&dir, pid, start, "watch-order").unwrap();
        std::fs::write(
            dir.join("status.json"),
            format!(
                r#"{{"state":"done","pid":{pid},"pid_start":{},"nonce":"watch-order"}}"#,
                start
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "null".to_string())
            ),
        )
        .unwrap();
    }

    #[test]
    fn watch_runs_dir_overrides_state_dir_in_either_flag_order() {
        let base = std::env::temp_dir().join(format!(
            "sfh-watch-flag-order-{}-{}",
            std::process::id(),
            crate::contain::random_nonce()
        ));
        let runs = base.join("runs");
        let state = base.join("state");
        terminal_run(&runs, "only-run");
        std::fs::create_dir_all(state.join("runs")).unwrap();

        for args in [
            vec![
                "--runs-dir".into(),
                runs.display().to_string(),
                "--state-dir".into(),
                state.display().to_string(),
            ],
            vec![
                "--state-dir".into(),
                state.display().to_string(),
                "--runs-dir".into(),
                runs.display().to_string(),
            ],
        ] {
            assert_eq!(cmd_watch(&args, Watch::Status), 0);
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn guide_fits_one_screen() {
        assert!(
            // Raised from 80 in v1.2.0: the guide is what an AI caller reads
            // to drive sfh, and v1.2 added the machine interface and the
            // workspace/context/replay keys it most needs to know about. Still
            // a budget, not a licence - anything added here has to earn a line.
            GUIDE.lines().count() <= 110,
            "guide is {} lines; maximum is 110",
            GUIDE.lines().count()
        );
    }

    #[test]
    fn every_guide_yaml_example_validates_and_dry_runs() {
        for (index, tail) in GUIDE.split("```yaml").skip(1).enumerate() {
            let yaml = tail
                .split("```")
                .next()
                .expect("guide YAML fence must be closed")
                .trim();
            let path =
                std::env::temp_dir().join(format!("sfh-guide-{}-{index}.yaml", std::process::id()));
            std::fs::write(&path, yaml).expect("write guide example");
            let validate_result = crate::engine::validate(&path, &[]);
            let runs_dir =
                std::env::temp_dir().join(format!("sfh-guide-{}-{index}-runs", std::process::id()));
            let run_result = crate::engine::run(crate::engine::RunOpts {
                flow_path: path.clone(),
                vars: Vec::new(),
                emit: None,
                runs_dir: Some(runs_dir.clone()),
                dry_run: true,
                verbose: false,
                quiet: true,
                resume: None,
                resume_latest: false,
                force_resume: false,
                no_partial_emit: false,
                detach: false,
                run_dir: None,
                ..Default::default()
            });
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir_all(&runs_dir);
            assert_eq!(
                validate_result,
                0,
                "guide YAML example {} is invalid",
                index + 1
            );
            assert_eq!(
                run_result,
                0,
                "guide YAML example {} does not dry-run",
                index + 1
            );
        }
    }

    #[test]
    fn plan_keeps_downstream_prompts_renderable_without_running_upstream_steps() {
        let path =
            std::env::temp_dir().join(format!("sfh-plan-dependency-{}.yaml", std::process::id()));
        std::fs::write(
            &path,
            r#"api_version: 1
name: plan-dependency
steps:
  - id: gather
    parallel:
      - id: left
        cmd: ["echo", "left"]
      - id: right
        cmd: ["echo", "right"]
  - id: verdict
    cmd: ["sh", "-c", "cat -"]
    stdin: prompt
    prompt: "{{steps.gather.outputs}}"
"#,
        )
        .expect("write plan fixture");
        let runs_dir =
            std::env::temp_dir().join(format!("sfh-plan-dependency-{}-runs", std::process::id()));
        let result = crate::engine::run(crate::engine::RunOpts {
            flow_path: path.clone(),
            vars: Vec::new(),
            emit: None,
            runs_dir: Some(runs_dir.clone()),
            dry_run: true,
            verbose: false,
            quiet: true,
            resume: None,
            resume_latest: false,
            force_resume: false,
            no_partial_emit: false,
            detach: false,
            run_dir: None,
            ..Default::default()
        });
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir_all(runs_dir);
        assert_eq!(
            result, 0,
            "plan must use placeholders for results that do not exist yet"
        );
    }

    #[test]
    fn nested_help_works_and_irrelevant_runs_arguments_are_rejected() {
        assert_eq!(
            cmd_config(&["show".into(), "--help".into()]),
            0,
            "config show has its own help"
        );
        for subcommand in ["list", "show", "why", "clean"] {
            assert_eq!(
                cmd_runs(&[subcommand.into(), "--help".into()]),
                0,
                "runs {subcommand} has its own help"
            );
        }
        assert_eq!(
            cmd_runs(&["list".into(), "ignored-run-dir".into()]),
            2,
            "runs list must not silently ignore a positional argument"
        );
        assert_eq!(
            cmd_runs(&["show".into(), "run".into(), "--keep".into(), "2".into()]),
            2,
            "runs show must not silently ignore clean-only options"
        );
        assert_eq!(
            cmd_init(&["one.yaml".into(), "two.yaml".into()]),
            2,
            "init must not silently use only the last output path"
        );
        assert_eq!(
            cmd_plan(&["flow.yaml".into(), "--resume-latest".into()]),
            2,
            "plan must reject run-only options instead of silently changing semantics"
        );
        for flag in ["-q", "--quiet", "-v", "--verbose"] {
            assert_eq!(
                cmd_plan(&["flow.yaml".into(), flag.into()]),
                2,
                "plan must reject accepted-looking output flag {flag}"
            );
        }
        for mode in [Watch::Status, Watch::Stop] {
            assert_eq!(
                cmd_watch(&["-q".into()], mode),
                2,
                "status and stop must reject a quiet flag they do not implement"
            );
        }
        assert_eq!(
            super::cmd_run(&[
                "flow.yaml".into(),
                "--resume".into(),
                "one".into(),
                "--resume-latest".into()
            ]),
            2,
            "two different resume selectors must not be silently prioritized"
        );
        assert_eq!(
            super::cmd_run(&["flow.yaml".into(), "--force-resume".into()]),
            2,
            "force-resume without a resume target is a user error"
        );
        assert_eq!(
            super::cmd_run(&["flow.yaml".into(), "-q".into(), "-v".into()]),
            2,
            "opposite output modes must not be order-dependent"
        );
        assert_eq!(
            super::cmd_preflight(&["--probe-binaries".into()]),
            2,
            "--probe-binaries without a flow file is a user error: a flowless survey has no bin: overrides to opt into probing"
        );
    }
}
