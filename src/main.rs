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
  sfh validate <flow.yaml> [--var k=v]   Parse and static-check a flow file
  sfh init [file] [--force]              Write an example flow file (default: flow.yaml)
  sfh guide                              Show the compact AI-oriented flow guide
  sfh runs list|show|clean [...]         Browse or prune past runs

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
  status exit codes: 0 = done, 1 = failed/dead/stopped, 2 = cannot tell, 3 = running
  wait exits with the flow's own code, or 3 if --timeout elapsed first
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
  runs clean [--runs-dir d] [--older-than DAYS] [--keep N] [--dry-run]

EXIT CODES:
  0 = flow succeeded    1 = flow failed    2 = config/usage error

FLOW FILE (see `sfh init` for a full example, schema/flow.schema.json for the schema):
  Steps run top-to-bottom unless a route: rule redirects. Templates:
  {{vars.name}} {{steps.<id>.output}} {{steps.<id>.outputs}} {{steps.<id>.output_file}}
  {{steps.<id>.exit}} {{steps.<id>.stderr_file}}
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
    if opts.detach && opts.dry_run {
        return usage_err(
            "--detach and --dry-run do nothing together (a dry run has nothing to detach)",
        );
    }
    engine::run(opts)
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
            "--json" if sub == "list" || sub == "show" => {
                as_json = true;
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
        "list" => runs::list(&runs_dir, limit, as_json),
        "show" => match target {
            Some(d) => runs::show(&d, as_json),
            None => usage_err("usage: sfh runs show <run-dir>"),
        },
        "clean" => runs::clean(&runs_dir, days, keep, dry),
        other => usage_err(&format!(
            "unknown runs subcommand '{other}' (list/show/clean)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::GUIDE;

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
}
