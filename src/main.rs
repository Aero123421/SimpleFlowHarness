mod engine;
mod execute;
mod flow;
mod leaf;
mod preset;
mod template;

use std::path::PathBuf;

const EXAMPLE: &str = include_str!("../examples/research.yaml");

const HELP: &str = "\
sfh - SimpleFlowHarness: chain AI CLI agents into staged flows

USAGE:
  sfh run <flow.yaml> [options]          Run a flow
  sfh validate <flow.yaml> [--var k=v]   Parse and static-check a flow file
  sfh init [file] [--force]              Write an example flow file (default: flow.yaml)

RUN OPTIONS:
  --var key=value     Override a flow variable (repeatable)
  --emit <step-id>    Print this step's output at the end (default: last executed step)
  --runs-dir <dir>    Where to store run artifacts (default: .sfh/runs)
  --dry-run           Render prompts/commands without executing anything
  -v, --verbose       Print full command lines
  -q, --quiet         Suppress progress output (stdout gets the result only)

EXIT CODES:
  0 = flow succeeded    1 = flow failed    2 = config/usage error

FLOW FILE (see `sfh init` for a full example):
  Steps run top-to-bottom unless a route: rule redirects. Templates:
  {{vars.name}} {{steps.<id>.output}} {{steps.<id>.output_file}}
  {{run_dir}} {{flow_dir}} {{step_id}} {{visit}} {{os}} {{prompt_file}}
  Preset tools: codex, claude, opencode, grok, agy.
  Custom cmd: array form = spawned directly; string form = via cmd /C | sh -c.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("run") => cmd_run(&args[1..]),
        Some("validate") => cmd_validate(&args[1..]),
        Some("init") => cmd_init(&args[1..]),
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

fn parse_vars_flag(rest: &[String], i: &mut usize, out: &mut Vec<(String, String)>) -> Result<(), String> {
    *i += 1;
    let kv = rest
        .get(*i)
        .ok_or_else(|| "--var needs key=value".to_string())?;
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
    };
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--var" => {
                if let Err(e) = parse_vars_flag(rest, &mut i, &mut opts.vars) {
                    return usage_err(&e);
                }
            }
            "--emit" => {
                i += 1;
                match rest.get(i) {
                    Some(v) => opts.emit = Some(v.clone()),
                    None => return usage_err("--emit needs a step id"),
                }
            }
            "--runs-dir" => {
                i += 1;
                match rest.get(i) {
                    Some(v) => opts.runs_dir = Some(PathBuf::from(v)),
                    None => return usage_err("--runs-dir needs a directory"),
                }
            }
            "--dry-run" => opts.dry_run = true,
            "-v" | "--verbose" => opts.verbose = true,
            "-q" | "--quiet" => opts.quiet = true,
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
        return usage_err("usage: sfh run <flow.yaml> [--var k=v]... [--emit id] [--runs-dir dir] [--dry-run] [-v] [-q]");
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
            eprintln!("next: edit it, then `sfh validate {0}` and `sfh run {0}`", path.display());
            0
        }
        Err(e) => usage_err(&format!("cannot write {}: {e}", path.display())),
    }
}
