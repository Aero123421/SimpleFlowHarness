mod contain;
mod doctor;
mod engine;
mod execute;
mod flow;
mod leaf;
mod preset;
mod runs;
mod sha256;
mod template;
mod watch;

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
  sfh runs list|show|why|clean [...]     Browse, explain or prune past runs

RUN OPTIONS:
  --var key=value     Override a flow variable (repeatable)
  --emit <step-id>    Print this step's output at the end (default: last executed step)
  --runs-dir <dir>    Where to store run artifacts (default: .sfh/runs)
  --detach            Run in the background, print the run dir, and exit at once.
                      The run survives this shell and its parent; poll it with
                      `sfh status` and collect it with `sfh wait`.
  --resume <run-dir>  Continue a previous run, reusing its finished steps
  --resume-latest     Same, picking the newest run dir
  --force-resume      Resume even though the flow file changed
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

DOCTOR OPTIONS:
  doctor [flow.yaml] [--runs-dir d] [--timeout SEC]
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

EXIT CODES:
  0 = flow succeeded    1 = flow failed    2 = config/usage error
  4 = flow stuck (a step routed to `goto: stuck`: work saved, needs a human)

FLOW FILE (see `sfh init` for a full example, schema/flow.schema.json for the schema):
  Steps run top-to-bottom unless a route: rule redirects. Templates:
  {{vars.name}} {{steps.<id>.output}} {{steps.<id>.outputs}} {{steps.<id>.output_file}}
  {{steps.<id>.exit}} {{steps.<id>.stderr_file}}
  {{item}} {{item_index}} {{notes}} {{run_dir}} {{flow_dir}} {{step_id}} {{visit}} {{os}}
  {{budget.spent_usd}} {{budget.elapsed_sec}} {{budget.remaining_usd}} {{budget.remaining_sec}}
  (remaining_* is the string `unlimited` when that axis has no ceiling)
  Filters: | head:N | tail:N | truncate:N | lines:A-B | trim | optional | default:text
  Preset tools: codex, claude, opencode, grok, agy, pi, cursor.
  Custom cmd: array form = spawned directly; string form = via cmd /C | sh -c.
";

fn main() {
    execute::install_process_guard();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some(cmd) if args.get(1).is_some_and(|a| a == "-h" || a == "--help") => cmd_help(cmd),
        Some("run") => cmd_run(&args[1..]),
        Some("plan") => cmd_plan(&args[1..]),
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
        Some("-h") | Some("--help") | Some("help") | None => {
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
        "run" => "sfh run <flow.yaml> [--var k=v] [--emit id] [--runs-dir d] [--detach] [--resume dir|--resume-latest] [--force-resume] [--no-partial-emit] [--dry-run] [-v|-q]",
        "plan" => "sfh plan <flow.yaml> [--var k=v] [-v|-q]\nResolves every command in an isolated temporary directory and executes nothing.",
        "graph" => "sfh graph <flow.yaml> [--mermaid]",
        "config" => "sfh config show <flow.yaml> [--show-secrets]",
        "validate" => "sfh validate <flow.yaml> [--var k=v] [--strict] [--json]",
        "status" => "sfh status [run-dir] [--runs-dir d] [--json]",
        "wait" => "sfh wait [run-dir] [--runs-dir d] [--timeout SEC] [--interval SEC] [-q]",
        "stop" => "sfh stop [run-dir] [--runs-dir d]",
        "doctor" => "sfh doctor [flow.yaml] [--runs-dir d] [--timeout SEC]",
        "init" => "sfh init [file] [--force]",
        "guide" => "sfh guide",
        "runs" => "sfh runs list|show|why|clean [options]",
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
    let mut opts = engine::RunOpts {
        flow_path: PathBuf::new(),
        vars: Vec::new(),
        emit: None,
        runs_dir: None,
        dry_run: false,
        verbose: false,
        quiet: false,
        resume: None,
        resume_latest: false,
        force_resume: false,
        no_partial_emit: false,
        detach: false,
        run_dir: None,
    };
    let mut i = 0;
    while i < rest.len() {
        let r = match rest[i].as_str() {
            "--var" => parse_vars_flag(rest, &mut i, &mut opts.vars),
            "--emit" => need(rest, &mut i, "--emit").map(|v| opts.emit = Some(v.clone())),
            "--runs-dir" => {
                need(rest, &mut i, "--runs-dir").map(|v| opts.runs_dir = Some(PathBuf::from(v)))
            }
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
    if opts.verbose && opts.quiet {
        return usage_err("--verbose and --quiet are mutually exclusive");
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
            "--var" => {
                if i + 1 >= rest.len() {
                    return usage_err("--var needs key=value");
                }
                i += 1;
            }
            "-v" | "--verbose" | "-q" | "--quiet" => {}
            flag if flag.starts_with('-') => {
                return usage_err(&format!(
                    "sfh plan does not accept run-only option '{flag}'"
                ))
            }
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
        println!("sfh config show <flow.yaml> [--show-secrets]");
        return 0;
    }
    if rest.first().map(String::as_str) != Some("show") {
        return usage_err("usage: sfh config show <flow.yaml> [--show-secrets]");
    }
    let mut path: Option<PathBuf> = None;
    let mut show_secrets = false;
    for arg in &rest[1..] {
        match arg.as_str() {
            "--show-secrets" => show_secrets = true,
            flag if flag.starts_with('-') => return usage_err(&format!("unknown flag '{flag}'")),
            value if path.is_some() => {
                return usage_err(&format!("more than one flow file given (extra: '{value}')"))
            }
            value => path = Some(PathBuf::from(value)),
        }
    }
    let Some(path) = path else {
        return usage_err("usage: sfh config show <flow.yaml> [--show-secrets]");
    };
    if show_secrets {
        eprintln!(
            "sfh: warning: --show-secrets prints environment values; treat stdout as sensitive"
        );
    }
    engine::show_config(&path, show_secrets)
}

#[derive(PartialEq, Clone, Copy)]
enum Watch {
    Status,
    Wait,
    Stop,
}

fn cmd_watch(rest: &[String], mode: Watch) -> i32 {
    let mut runs_dir = PathBuf::from(".sfh").join("runs");
    let mut target: Option<PathBuf> = None;
    let mut as_json = false;
    let mut quiet = false;
    let mut timeout: Option<u64> = None;
    let mut interval = 3u64;
    let is_wait = mode == Watch::Wait;
    let mut i = 0;
    while i < rest.len() {
        let r: Result<(), String> = match rest[i].as_str() {
            "--runs-dir" => need(rest, &mut i, "--runs-dir").map(|v| runs_dir = PathBuf::from(v)),
            "--json" if mode == Watch::Status => {
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
            "-q" | "--quiet" => {
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
    match mode {
        Watch::Wait => watch::wait(target.as_deref(), &runs_dir, timeout, interval, quiet),
        Watch::Stop => watch::stop(target.as_deref(), &runs_dir),
        Watch::Status => watch::status(target.as_deref(), &runs_dir, as_json),
    }
}

fn cmd_doctor(rest: &[String]) -> i32 {
    let mut flow_path: Option<PathBuf> = None;
    let mut runs_dir = PathBuf::from(".sfh").join("runs");
    let mut timeout = 120u64;
    let mut i = 0;
    while i < rest.len() {
        let r: Result<(), String> = match rest[i].as_str() {
            "--runs-dir" => need(rest, &mut i, "--runs-dir").map(|v| runs_dir = PathBuf::from(v)),
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
    let work = doctor::default_work_dir(&runs_dir);
    doctor::run(flow_path.as_deref(), timeout, &work)
}

fn cmd_validate(rest: &[String]) -> i32 {
    let mut flow_path: Option<PathBuf> = None;
    let mut vars: Vec<(String, String)> = Vec::new();
    let mut strict = false;
    let mut as_json = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--var" => {
                if let Err(e) = parse_vars_flag(rest, &mut i, &mut vars) {
                    return usage_err(&e);
                }
            }
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
    if strict || as_json {
        engine::validate_with_options(&fp, &vars, strict, as_json)
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
    if rest
        .iter()
        .skip(1)
        .any(|arg| arg == "-h" || arg == "--help")
    {
        let usage = match sub {
            "list" => "sfh runs list [--runs-dir d] [-n N] [--json]",
            "show" => "sfh runs show <run-dir> [--json]",
            "why" => "sfh runs why <run-dir> [--json]",
            "clean" => "sfh runs clean [--runs-dir d] [--older-than DAYS] [--keep N] [--dry-run]",
            _ => "sfh runs list|show|why|clean [options]",
        };
        println!("{usage}");
        return if matches!(sub, "list" | "show" | "why" | "clean") {
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
            "-n" | "--limit" if sub == "list" => need(rest, &mut i, "-n")
                .and_then(|v| v.parse().map_err(|_| "-n needs a number".to_string()))
                .map(|v| limit = v),
            "--older-than" if sub == "clean" => need(rest, &mut i, "--older-than")
                .and_then(|v| {
                    v.trim_end_matches('d')
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
    use super::{cmd_config, cmd_init, cmd_plan, cmd_runs, GUIDE};

    #[test]
    fn guide_fits_one_screen() {
        assert!(
            GUIDE.lines().count() <= 80,
            "guide is {} lines; maximum is 80",
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
    }
}
