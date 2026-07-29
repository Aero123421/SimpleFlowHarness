//! `sfh doctor` - prove the presets still match the CLIs that are installed.
//!
//! Every preset encodes flags and output shapes that were true on the day they
//! were verified against a live tool. Those CLIs ship weekly. Without a check,
//! drift surfaces as a flow that dies halfway through a paid run - or worse, as
//! a permission flag the tool quietly stopped honouring.
//!
//! So this does the one thing a static check cannot: it actually runs each tool
//! the way a step runs it, with a one-token prompt, and asserts that the parser
//! still finds the answer. Cheap, but it is a real call to a real model, so it
//! is a command the user chooses to run, never something a flow does silently.

use crate::{execute, flow, leaf, preset};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Deliberately trivial: the point is the round trip, not the reasoning.
const PROBE: &str = "Reply with exactly this and nothing else: SFH-OK";
const MARKER: &str = "SFH-OK";

struct Report {
    tool: String,
    /// The binary the preset actually launches. NOT the tool name: `cursor`
    /// runs `cursor-agent`, and probing `cursor` starts the Electron editor.
    program: String,
    version: Option<String>,
    /// None when the tool was never launched (not installed).
    outcome: Option<Result<Detail, String>>,
    required: bool,
}

struct Detail {
    said_marker: bool,
    text: String,
    session: bool,
    usage: bool,
    dur_ms: u128,
}

/// `flow_path`: when given, check exactly the tools that flow uses, resolved
/// through its profiles (so a machine-local `bin:` is honoured), and treat a
/// missing tool as a failure. Without it, probe every preset and merely report
/// which ones are absent.
/// What to probe. `bin` is None unless a profile pins one, in which case the
/// preset's own program name is used.
struct Target {
    tool: String,
    bin: Option<String>,
    model: Option<String>,
    effort: Option<String>,
}

pub fn run(flow_path: Option<&Path>, timeout_sec: u64, work: &Path) -> i32 {
    let mut targets: Vec<Target> = Vec::new();
    let mut required = false;

    match flow_path {
        Some(p) => {
            required = true;
            let f = match flow::load(p) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("sfh: {e}");
                    return 2;
                }
            };
            // Exactly the (tool, bin, model, effort) tuples the flow resolves
            // to - same resolution the engine uses. Unused profiles are never
            // probed, so a profile's bin is never launched just by existing.
            for rt in f.resolved_tools() {
                targets.push(Target {
                    tool: rt.tool,
                    bin: rt.bin,
                    model: rt.model,
                    effort: rt.effort,
                });
            }
            if targets.is_empty() {
                eprintln!(
                    "sfh: {} uses no preset tools (cmd: steps only)",
                    p.display()
                );
                return 0;
            }
        }
        None => {
            for t in flow::TOOLS {
                targets.push(Target {
                    tool: t.to_string(),
                    bin: None,
                    model: None,
                    effort: None,
                });
            }
        }
    }

    if let Err(e) = crate::contain::mkdir_private(work) {
        eprintln!("sfh: cannot create {}: {e}", work.display());
        return 2;
    }
    eprintln!(
        "sfh: probing {} tool(s) with a one-token prompt each; this makes real calls",
        targets.len()
    );

    let mut reports = Vec::new();
    for Target {
        tool,
        bin,
        model,
        effort,
    } in targets
    {
        // Build first: only the preset knows which binary it launches.
        let built = build_probe(&tool, bin.as_deref(), model, effort, timeout_sec, work);
        let (program, built) = match built {
            Ok((p, b)) => (p, Some(b)),
            Err(e) => {
                let r = Report {
                    tool,
                    program: bin.unwrap_or_default(),
                    version: None,
                    outcome: Some(Err(e)),
                    required,
                };
                print_row(&r);
                reports.push(r);
                continue;
            }
        };
        let version = execute::probe_version(&program);
        let outcome = if version.is_none() && !required {
            None
        } else {
            Some(run_probe(built.expect("built"), timeout_sec))
        };
        let r = Report {
            tool,
            program,
            version,
            outcome,
            required,
        };
        print_row(&r);
        reports.push(r);
    }

    let failed: Vec<&Report> = reports
        .iter()
        .filter(|r| matches!(&r.outcome, Some(Err(_))) || (r.required && r.version.is_none()))
        .collect();
    let degraded: Vec<&Report> = reports
        .iter()
        .filter(|r| matches!(&r.outcome, Some(Ok(d)) if !d.said_marker))
        .collect();

    eprintln!();
    if !failed.is_empty() {
        eprintln!(
            "sfh: {} preset(s) BROKEN: {}",
            failed.len(),
            failed
                .iter()
                .map(|r| r.tool.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        eprintln!(
            "sfh: the CLI most likely changed its flags or output shape. Work around it per step\n\
             sfh: with args: [...] or cmd: [...], and please open an issue so the preset can catch up."
        );
        return 1;
    }
    if !degraded.is_empty() {
        eprintln!(
            "sfh: {} tool(s) answered but not with the exact text asked for: {}. \
             The preset works; the model was just chatty.",
            degraded.len(),
            degraded
                .iter()
                .map(|r| r.tool.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    eprintln!("sfh: all probed presets are working.");
    0
}

fn print_row(r: &Report) {
    // Always name the binary: "cursor" running "cursor-agent" is exactly the
    // kind of mismatch this command exists to make visible.
    let ver = match &r.version {
        Some(v) => format!("{} ({v})", r.program),
        None => format!("{} (not found)", r.program),
    };
    match &r.outcome {
        None => println!("{:<9} {:<10} {ver}", r.tool, "SKIP"),
        Some(Err(e)) => {
            println!("{:<9} {:<10} {ver}", r.tool, "BROKEN");
            println!("{:<9} {:<10} {e}", "", "");
        }
        Some(Ok(d)) => {
            let mut notes = Vec::new();
            notes.push(if d.said_marker {
                "text ok".to_string()
            } else {
                format!("text ok (said {:?})", one_line(&d.text))
            });
            notes.push(format!("session {}", if d.session { "ok" } else { "none" }));
            notes.push(format!("usage {}", if d.usage { "ok" } else { "none" }));
            notes.push(format!("{:.1}s", d.dur_ms as f64 / 1000.0));
            println!(
                "{:<9} {:<10} {ver}\n{:<9} {:<10} {}",
                r.tool,
                "OK",
                "",
                "",
                notes.join(", ")
            );
        }
    }
}

fn one_line(s: &str) -> String {
    let t: String = s
        .replace(['\n', '\r'], " ")
        .trim()
        .chars()
        .take(48)
        .collect();
    t
}

/// Returns (program actually launched, the built invocation).
fn build_probe(
    tool: &str,
    bin: Option<&str>,
    model: Option<String>,
    effort: Option<String>,
    timeout_sec: u64,
    work: &Path,
) -> Result<(String, preset::Built), String> {
    let last = work.join(format!("{tool}.last.txt"));
    let pfile = work.join(format!("{tool}.prompt.txt"));
    crate::contain::write_private(&pfile, PROBE)
        .map_err(|e| format!("cannot write probe prompt: {e}"))?;
    let _ = std::fs::remove_file(&last);

    let built = preset::build(
        tool,
        preset::PresetInput {
            model,
            effort,
            // read: a health check must not be able to touch anything.
            access: preset::Access::Read,
            agent: None,
            extra: &[],
            bin: bin.map(String::from),
            timeout_sec: Some(timeout_sec),
        },
        &preset::BuildPaths {
            last_msg: &last,
            prompt_file: &pfile,
        },
        None,
    )?;
    let program = built.argv.first().cloned().unwrap_or_default();
    Ok((program, built))
}

fn run_probe(built: preset::Built, timeout_sec: u64) -> Result<Detail, String> {
    let mut argv = built.argv;
    let stdin_payload = match built.delivery {
        preset::Delivery::Stdin => Some(PROBE.as_bytes().to_vec()),
        preset::Delivery::PromptFile | preset::Delivery::None => None,
        preset::Delivery::Arg => {
            argv.push(PROBE.to_string());
            None
        }
    };
    let out = execute::run_cmd(
        &execute::Invocation::Argv(argv),
        stdin_payload,
        None,
        Some(Duration::from_secs(timeout_sec)),
        &built.env_remove,
        &built.env_set,
        execute::Observe::default(),
    )?;
    if out.timed_out {
        return Err(format!("no answer within {timeout_sec}s"));
    }

    let stdout = leaf::clean_text(&out.stdout);
    let stderr = leaf::clean_text(&out.stderr);
    // None run dir: doctor's scratch files live in a dir sfh itself created and
    // cleared (build_probe), not an untrusted resumed run dir, so a plain read.
    let parsed = leaf::parse_output(&built.parse, &stdout, &stderr, None)?;
    if parsed.failed || (out.exit_code != 0 && parsed.text.is_empty()) {
        let why = leaf::tail_lines(&stderr, 3).join(" | ");
        return Err(format!(
            "exit {} and no parseable answer{}",
            out.exit_code,
            if why.is_empty() {
                String::new()
            } else {
                format!(": {why}")
            }
        ));
    }
    if parsed.text.trim().is_empty() {
        return Err(
            "the tool ran but sfh could not extract any text - its output format changed".into(),
        );
    }
    let usage = parsed.usage.input_tokens.is_some()
        || parsed.usage.output_tokens.is_some()
        || parsed.usage.cost_usd.is_some();
    Ok(Detail {
        said_marker: parsed.text.to_uppercase().contains(MARKER),
        text: parsed.text,
        // Not every tool reports one on a fresh run; absence is informational,
        // but it does mean continue_from/fork_from cannot work for that tool.
        session: parsed.session.is_some() || built.preassigned_session.is_some(),
        usage,
        dur_ms: out.dur_ms,
    })
}

/// Where doctor keeps its scratch prompt/last-message files.
pub fn default_work_dir(runs_dir: &Path) -> PathBuf {
    runs_dir.join(".doctor")
}
