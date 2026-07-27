mod engine;
mod execute;
mod flow;
mod leaf;
mod preset;
mod runs;
mod template;

use std::path::PathBuf;

const EXAMPLE: &str = include_str!("../examples/research.yaml");

const HELP: &str = "\
sfh - SimpleFlowHarness: chain AI CLI agents into staged flows

USAGE:
  sfh run <flow.yaml> [options]          Run a flow
  sfh validate <flow.yaml> [--var k=v]   Parse and static-check a flow file
  sfh init [file] [--force]              Write an example flow file (default: flow.yaml)
  sfh runs list|show|clean [...]         Browse or prune past runs

RUN OPTIONS:
  --var key=value     Override a flow variable (repeatable)
  --emit <step-id>    Print this step's output at the end (default: last executed step)
  --runs-dir <dir>    Where to store run artifacts (default: .sfh/runs)
  --resume <run-dir>  Continue a previous run, reusing its finished steps
  --resume-latest     Same, picking the newest run dir
  --force-resume      Resume even though the flow file changed
  --no-partial-emit   On failure, do not print the best available output
  --dry-run           Render prompts/commands without executing anything
  -v, --verbose       Print full command lines
  -q, --quiet         Suppress progress output (stdout gets the result only)

RUNS OPTIONS:
  runs list [--runs-dir d] [-n N]
  runs show <run-dir>
  runs clean [--runs-dir d] [--older-than DAYS] [--keep N] [--dry-run]

EXIT CODES:
  0 = flow succeeded    1 = flow failed    2 = config/usage error

FLOW FILE (see `sfh init` for a full example, schema/flow.schema.json for the schema):
  Steps run top-to-bottom unless a route: rule redirects. Templates:
  {{vars.name}} {{steps.<id>.output}} {{steps.<id>.outputs}} {{steps.<id>.output_file}}
  {{item}} {{item_index}} {{notes}} {{run_dir}} {{flow_dir}} {{step_id}} {{visit}} {{os}}
  Filters: | head:N | tail:N | truncate:N | lines:A-B | trim
  Preset tools: codex, claude, opencode, grok, agy, pi, cursor.
  Custom cmd: array form = spawned directly; string form = via cmd /C | sh -c.
";

fn main() {
    execute::install_process_guard();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("run") => cmd_run(&args[1..]),
        Some("validate") => cmd_validate(&args[1..]),
        Some("init") => cmd_init(&args[1..]),
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
    engine::run(opts)
}

fn cmd_validate(rest: &[String]) -> i32 {
    let mut flow_path: Option<PathBuf> = None;
    let mut vars: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--var" => {
                if let Err(e) = parse_vars_flag(rest, &mut i, &mut vars) {
                    return usage_err(&e);
                }
            }
            s if s.starts_with('-') => return usage_err(&format!("unknown flag '{s}'")),
            s => flow_path = Some(PathBuf::from(s)),
        }
        i += 1;
    }
    let Some(fp) = flow_path else {
        return usage_err("usage: sfh validate <flow.yaml> [--var k=v]...");
    };
    engine::validate(&fp, &vars)
}

fn cmd_init(rest: &[String]) -> i32 {
    let mut path = PathBuf::from("flow.yaml");
    let mut force = false;
    for a in rest {
        match a.as_str() {
            "--force" => force = true,
            s if s.starts_with('-') => return usage_err(&format!("unknown flag '{s}'")),
            s => path = PathBuf::from(s),
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
                path.display()
            );
            0
        }
        Err(e) => usage_err(&format!("cannot write {}: {e}", path.display())),
    }
}

fn cmd_runs(rest: &[String]) -> i32 {
    let mut runs_dir = PathBuf::from(".sfh").join("runs");
    let mut limit = 20usize;
    let mut days = 30u64;
    let mut keep = 5usize;
    let mut dry = false;
    let mut target: Option<PathBuf> = None;
    let sub = rest.first().map(String::as_str).unwrap_or("list");
    let mut i = 1;
    while i < rest.len() {
        let r: Result<(), String> = match rest[i].as_str() {
            "--runs-dir" => need(rest, &mut i, "--runs-dir").map(|v| runs_dir = PathBuf::from(v)),
            "-n" | "--limit" => need(rest, &mut i, "-n")
                .and_then(|v| v.parse().map_err(|_| "-n needs a number".to_string()))
                .map(|v| limit = v),
            "--older-than" => need(rest, &mut i, "--older-than")
                .and_then(|v| {
                    v.trim_end_matches('d')
                        .parse()
                        .map_err(|_| "--older-than needs days".to_string())
                })
                .map(|v| days = v),
            "--keep" => need(rest, &mut i, "--keep")
                .and_then(|v| v.parse().map_err(|_| "--keep needs a number".to_string()))
                .map(|v| keep = v),
            "--dry-run" => {
                dry = true;
                Ok(())
            }
            s if s.starts_with('-') => Err(format!("unknown flag '{s}'")),
            s => {
                target = Some(PathBuf::from(s));
                Ok(())
            }
        };
        if let Err(e) = r {
            return usage_err(&e);
        }
        i += 1;
    }
    match sub {
        "list" => runs::list(&runs_dir, limit),
        "show" => match target {
            Some(d) => runs::show(&d),
            None => usage_err("usage: sfh runs show <run-dir>"),
        },
        "clean" => runs::clean(&runs_dir, days, keep, dry),
        other => usage_err(&format!(
            "unknown runs subcommand '{other}' (list/show/clean)"
        )),
    }
}
