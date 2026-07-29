use std::collections::{BTreeMap, HashSet};

/// Latest captured result of a step, exposed to templates.
#[derive(Clone, Debug)]
pub struct StepOutput {
    /// Chain output. For parallel/foreach steps this is the aggregated output.
    pub output: String,
    /// Aggregated output ("--- id ---" separated). Equals `output` for plain steps.
    pub outputs: String,
    pub output_file: String,
    /// Exit code normalized by sfh after parsing and session validation.
    pub exit: i32,
    pub stderr_file: String,
}

pub struct Ctx<'a> {
    pub vars: &'a BTreeMap<String, String>,
    pub outputs: &'a BTreeMap<String, StepOutput>,
    pub step_ids: &'a HashSet<String>,
    pub builtins: BTreeMap<String, String>,
}

/// Render `{{ key | filter:arg | ... }}` placeholders.
///
/// Keys:
///   vars.NAME | steps.ID.output | steps.ID.outputs | steps.ID.output_file
///   steps.ID.exit | steps.ID.stderr_file
///   run_dir, flow_dir, step_id, visit, os, prompt_file, notes, item, item_index
/// Filters:
///   head:N (first N lines) | tail:N (last N lines) | truncate:N (first N chars)
///   lines:A-B (1-indexed inclusive) | trim | optional | default:text
/// Referencing a step that exists but has not run yet yields an empty string.
/// Mark intentional branch-optional references with `optional`; strict
/// validation can then distinguish them from accidental missing dependencies.
pub fn render(input: &str, ctx: &Ctx) -> Result<String, String> {
    render_impl(input, ctx, None)
}

/// Callback run over every substituted value: (template key, rendered value).
pub type SubstCheck<'a> = &'a dyn Fn(&str, &str) -> Result<(), String>;

/// Like render, but every substituted value is passed to `check(key, value)`
/// before insertion. Used for shell-string cmd: to reject values that would be
/// re-parsed by cmd.exe / sh.
pub fn render_checked(input: &str, ctx: &Ctx, check: SubstCheck) -> Result<String, String> {
    render_impl(input, ctx, Some(check))
}

const RAW_END: &str = "{{endraw}}";

/// Call `check(key)` for every template key that rendering `input` would
/// substitute (raw blocks skipped, filters stripped). Used to statically
/// validate executed-privileged fields (bin / cwd / argv[0]) at load time,
/// when the substituted VALUES are not yet known but the KEYS already reveal
/// whether step output or other run-derived data would flow in (rev_break #12,
/// rev_regression: validator must reject what the runtime rejects).
pub fn check_keys(
    input: &str,
    mut check: impl FnMut(&str) -> Result<(), String>,
) -> Result<(), String> {
    let mut rest = input;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        if let Some(body) = after.strip_prefix("raw}}") {
            match body.find(RAW_END) {
                Some(end) => {
                    rest = &body[end + RAW_END.len()..];
                    continue;
                }
                None => return Ok(()), // unclosed raw: a render error, reported elsewhere
            }
        }
        let Some(end) = after.find("}}") else {
            return Ok(()); // unclosed: a render error, reported elsewhere
        };
        let inner = &after[..end];
        let key = inner.split('|').next().unwrap_or("").trim();
        check(key)?;
        rest = &after[end + 2..];
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRef {
    pub id: String,
    /// `optional` and `default:` are explicit acknowledgements that the source
    /// may not have run on this control-flow path.
    pub optional: bool,
}

/// Collect step-output dependencies from a template without rendering it.
/// Raw blocks are skipped exactly as render/check_keys skip them.
pub fn step_refs(input: &str) -> Vec<StepRef> {
    let mut refs = Vec::new();
    let mut rest = input;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        if let Some(body) = after.strip_prefix("raw}}") {
            match body.find(RAW_END) {
                Some(end) => {
                    rest = &body[end + RAW_END.len()..];
                    continue;
                }
                None => break,
            }
        }
        let Some(end) = after.find("}}") else {
            break;
        };
        let inner = &after[..end];
        let mut parts = inner.split('|');
        let key = parts.next().unwrap_or("").trim();
        if let Some(step) = key.strip_prefix("steps.") {
            let id = step.split('.').next().unwrap_or("");
            if !id.is_empty() {
                let optional = parts.any(|filter| {
                    let name = filter
                        .trim()
                        .split_once(':')
                        .map(|(name, _)| name)
                        .unwrap_or_else(|| filter.trim());
                    name == "optional" || name == "default"
                });
                refs.push(StepRef {
                    id: id.to_string(),
                    optional,
                });
            }
        }
        rest = &after[end + 2..];
    }
    refs
}

/// True when rendering `input` would substitute at least one value. Raw blocks
/// ({{raw}}...{{endraw}}) pass their body through untouched and do not count.
/// Used to fail-closed on template expansion inside string-form cmd:.
pub fn contains_template(input: &str) -> bool {
    let mut rest = input;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        if let Some(body) = after.strip_prefix("raw}}") {
            match body.find(RAW_END) {
                // Skip the raw body; what follows may still contain templates.
                Some(end) => {
                    rest = &body[end + RAW_END.len()..];
                    continue;
                }
                // Unclosed raw is a render error anyway; treat as a template
                // so validation rejects it early instead of at spawn time.
                None => return true,
            }
        }
        return true;
    }
    false
}

fn render_impl(input: &str, ctx: &Ctx, check: Option<SubstCheck>) -> Result<String, String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        // {{raw}} ... {{endraw}}: copy the body through untouched. A prompt
        // that talks ABOUT templates - handlebars, jinja, another sfh flow -
        // needs some way to write {{ without sfh trying to resolve it, and
        // "put it in a file" is not an answer for a coding tool.
        if let Some(body) = after.strip_prefix("raw}}") {
            let end = body.find(RAW_END).ok_or_else(|| {
                format!(
                    "unclosed '{{{{raw}}}}' near: {:.40} (close it with '{RAW_END}')",
                    &rest[start..]
                )
            })?;
            out.push_str(&body[..end]);
            rest = &body[end + RAW_END.len()..];
            continue;
        }
        let end = after
            .find("}}")
            .ok_or_else(|| format!("unclosed '{{{{' near: {:.40}", &rest[start..]))?;
        let inner = &after[..end];
        let mut parts = inner.split('|');
        let key = parts.next().unwrap_or("").trim();
        let mut val = lookup(key, ctx)?;
        for f in parts {
            val = apply_filter(val, f.trim()).map_err(|e| format!("in '{{{{{inner}}}}}': {e}"))?;
        }
        if let Some(chk) = check {
            chk(key, &val)?;
        }
        out.push_str(&val);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn lookup(key: &str, ctx: &Ctx) -> Result<String, String> {
    if let Some(name) = key.strip_prefix("vars.") {
        return ctx.vars.get(name).cloned().ok_or_else(|| {
            format!("undefined variable '{name}' (define it under vars: or pass --var {name}=...)")
        });
    }
    if let Some(rest) = key.strip_prefix("steps.") {
        let mut it = rest.splitn(2, '.');
        let id = it.next().unwrap_or("");
        let field = it.next().unwrap_or("output");
        if !ctx.step_ids.contains(id) {
            return Err(format!("unknown step id '{id}' in template"));
        }
        let so = ctx.outputs.get(id);
        return match field {
            "output" => Ok(so.map(|s| s.output.clone()).unwrap_or_default()),
            "outputs" => Ok(so.map(|s| s.outputs.clone()).unwrap_or_default()),
            "output_file" => Ok(so.map(|s| s.output_file.clone()).unwrap_or_default()),
            "exit" => Ok(so.map(|s| s.exit.to_string()).unwrap_or_default()),
            "stderr_file" => Ok(so.map(|s| s.stderr_file.clone()).unwrap_or_default()),
            other => Err(format!(
                "unknown field 'steps.{id}.{other}' (use output, outputs, output_file, exit or stderr_file)"
            )),
        };
    }
    if let Some(v) = ctx.builtins.get(key) {
        return Ok(v.clone());
    }
    Err(format!("unknown template key '{{{{{key}}}}}'"))
}

fn apply_filter(val: String, f: &str) -> Result<String, String> {
    let (name, arg) = match f.split_once(':') {
        Some((n, a)) => (n.trim(), Some(a.trim())),
        None => (f, None),
    };
    let num = |a: Option<&str>| -> Result<usize, String> {
        a.ok_or_else(|| format!("filter '{name}' needs an argument"))?
            .parse::<usize>()
            .map_err(|_| format!("filter '{name}': argument must be a number"))
    };
    match name {
        "trim" => Ok(val.trim().to_string()),
        "optional" => {
            if arg.is_some() {
                Err("filter 'optional' takes no argument".into())
            } else {
                Ok(val)
            }
        }
        "default" => {
            let fallback = arg.ok_or("filter 'default' needs text")?;
            if val.is_empty() {
                Ok(fallback.to_string())
            } else {
                Ok(val)
            }
        }
        "head" => {
            let n = num(arg)?;
            Ok(val.lines().take(n).collect::<Vec<_>>().join("\n"))
        }
        "tail" => {
            let n = num(arg)?;
            let lines: Vec<&str> = val.lines().collect();
            let start = lines.len().saturating_sub(n);
            Ok(lines[start..].join("\n"))
        }
        "truncate" => {
            let n = num(arg)?;
            let total = val.chars().count();
            if total <= n {
                Ok(val)
            } else {
                let cut: String = val.chars().take(n).collect();
                Ok(format!(
                    "{cut}\n...[truncated {} of {total} chars]",
                    total - n
                ))
            }
        }
        "lines" => {
            let a = arg.ok_or("filter 'lines' needs A-B")?;
            let (s, e) = a
                .split_once('-')
                .ok_or("filter 'lines' needs A-B (1-indexed, inclusive)")?;
            let s: usize = s
                .trim()
                .parse()
                .map_err(|_| "lines: bad start".to_string())?;
            let e: usize = e.trim().parse().map_err(|_| "lines: bad end".to_string())?;
            if s == 0 || e < s {
                return Err("lines: need 1 <= A <= B".into());
            }
            Ok(val
                .lines()
                .skip(s - 1)
                .take(e - s + 1)
                .collect::<Vec<_>>()
                .join("\n"))
        }
        other => Err(format!(
            "unknown filter '{other}' (head/tail/truncate/lines/trim/optional/default)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with(
        outputs: BTreeMap<String, StepOutput>,
    ) -> (
        BTreeMap<String, String>,
        HashSet<String>,
        BTreeMap<String, StepOutput>,
    ) {
        let vars = BTreeMap::from([("topic".to_string(), "rust".to_string())]);
        let ids: HashSet<String> = outputs.keys().cloned().chain(["gen".to_string()]).collect();
        (vars, ids, outputs)
    }

    fn out(text: &str) -> StepOutput {
        StepOutput {
            output: text.to_string(),
            outputs: text.to_string(),
            output_file: "/run/gen.out.txt".to_string(),
            exit: 7,
            stderr_file: "/run/gen.err.txt".to_string(),
        }
    }

    fn render_with(tpl: &str, outputs: BTreeMap<String, StepOutput>) -> Result<String, String> {
        let (vars, ids, outputs) = ctx_with(outputs);
        let ctx = Ctx {
            vars: &vars,
            outputs: &outputs,
            step_ids: &ids,
            builtins: BTreeMap::from([("run_dir".to_string(), "/run".to_string())]),
        };
        render(tpl, &ctx)
    }

    #[test]
    fn raw_blocks_pass_braces_through_untouched() {
        let o = BTreeMap::new();
        // The motivating case: asking an agent to fix someone else's template.
        assert_eq!(
            render_with(
                "fix this: {{raw}}{{user.name}} and {{#each xs}}{{endraw}} please",
                o.clone()
            )
            .unwrap(),
            "fix this: {{user.name}} and {{#each xs}} please"
        );
        // Substitution still works on either side of a raw block.
        assert_eq!(
            render_with(
                "{{vars.topic}} {{raw}}{{x}}{{endraw}} {{vars.topic}}",
                o.clone()
            )
            .unwrap(),
            "rust {{x}} rust"
        );
        let e = render_with("{{raw}}{{x}} never closed", o).unwrap_err();
        assert!(e.contains("endraw"), "{e}");
    }

    #[test]
    fn renders_vars_steps_and_builtins() {
        let o = BTreeMap::from([("gen".to_string(), out("a\nb\nc"))]);
        assert_eq!(render_with("{{vars.topic}}", o.clone()).unwrap(), "rust");
        assert_eq!(
            render_with("{{steps.gen.output}}", o.clone()).unwrap(),
            "a\nb\nc"
        );
        assert_eq!(render_with("{{run_dir}}", o.clone()).unwrap(), "/run");
        assert_eq!(
            render_with("{{steps.gen.output_file}}", o).unwrap(),
            "/run/gen.out.txt"
        );
        let o = BTreeMap::from([("gen".to_string(), out("a\nb\nc"))]);
        assert_eq!(render_with("{{steps.gen.exit}}", o.clone()).unwrap(), "7");
        assert_eq!(
            render_with("{{steps.gen.stderr_file}}", o).unwrap(),
            "/run/gen.err.txt"
        );
    }

    #[test]
    fn unrun_step_renders_empty_but_unknown_step_errors() {
        assert_eq!(
            render_with("[{{steps.gen.output}}]", BTreeMap::new()).unwrap(),
            "[]"
        );
        let e = render_with("{{steps.nope.output}}", BTreeMap::new()).unwrap_err();
        assert!(e.contains("unknown step id"), "{e}");
    }

    #[test]
    fn filters_slice_text() {
        let o = BTreeMap::from([("gen".to_string(), out("l1\nl2\nl3\nl4"))]);
        assert_eq!(
            render_with("{{steps.gen.output | head:2}}", o.clone()).unwrap(),
            "l1\nl2"
        );
        assert_eq!(
            render_with("{{steps.gen.output | tail:2}}", o.clone()).unwrap(),
            "l3\nl4"
        );
        assert_eq!(
            render_with("{{steps.gen.output | lines:2-3}}", o.clone()).unwrap(),
            "l2\nl3"
        );
        assert_eq!(
            render_with("{{steps.gen.output | head:0}}", o.clone()).unwrap(),
            ""
        );
        assert_eq!(
            render_with("{{steps.gen.output | tail:99}}", o.clone()).unwrap(),
            "l1\nl2\nl3\nl4"
        );
        assert!(render_with("{{steps.gen.output | truncate:5}}", o.clone())
            .unwrap()
            .starts_with("l1\nl2"));
        assert!(render_with("{{steps.gen.output | truncate:5}}", o.clone())
            .unwrap()
            .contains("truncated"));
        // chaining, and trim
        assert_eq!(
            render_with("{{steps.gen.output | head:1 | trim}}", o).unwrap(),
            "l1"
        );
    }

    #[test]
    fn optional_and_default_filters_make_branch_intent_explicit() {
        assert_eq!(
            render_with("[{{steps.gen.output | optional}}]", BTreeMap::new()).unwrap(),
            "[]"
        );
        assert_eq!(
            render_with("{{steps.gen.output | default:not-run}}", BTreeMap::new()).unwrap(),
            "not-run"
        );
        let present = BTreeMap::from([("gen".to_string(), out("value"))]);
        assert_eq!(
            render_with("{{steps.gen.output | default:not-run}}", present).unwrap(),
            "value"
        );

        let refs = step_refs(
            "{{steps.a.output}} {{steps.b.output | optional}} \
             {{steps.c.output | default:no}} {{raw}}{{steps.d.output}}{{endraw}}",
        );
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].id, "a");
        assert!(!refs[0].optional);
        assert_eq!(refs[1].id, "b");
        assert!(refs[1].optional);
        assert_eq!(refs[2].id, "c");
        assert!(refs[2].optional);
    }

    #[test]
    fn filter_errors_are_reported_with_context() {
        let o = BTreeMap::from([("gen".to_string(), out("x"))]);
        assert!(render_with("{{steps.gen.output | nope:1}}", o.clone())
            .unwrap_err()
            .contains("unknown filter"));
        assert!(render_with("{{steps.gen.output | head:x}}", o.clone())
            .unwrap_err()
            .contains("must be a number"));
        assert!(render_with("{{steps.gen.output | lines:3-1}}", o)
            .unwrap_err()
            .contains("1 <= A <= B"));
    }

    #[test]
    fn undefined_var_and_unclosed_brace_error() {
        assert!(render_with("{{vars.missing}}", BTreeMap::new())
            .unwrap_err()
            .contains("undefined variable"));
        assert!(render_with("{{vars.topic", BTreeMap::new())
            .unwrap_err()
            .contains("unclosed"));
    }

    #[test]
    fn contains_template_sees_substitutions_but_not_raw_blocks() {
        assert!(contains_template("echo {{vars.x}}"));
        assert!(contains_template("{{steps.a.output}}"));
        assert!(contains_template(
            "plain {{raw}}literal{{endraw}} then {{vars.x}}"
        ));
        assert!(!contains_template("echo plain"));
        assert!(!contains_template(
            "echo {{raw}}{{steps.a.output}}{{endraw}}"
        ));
        assert!(!contains_template(""));
        // An unclosed raw block is a render error; count it as a template so
        // validation rejects it up front.
        assert!(contains_template("echo {{raw}}never closed"));
    }

    #[test]
    fn render_checked_can_reject_substituted_values() {
        let (vars, ids, outputs) =
            ctx_with(BTreeMap::from([("gen".to_string(), out("evil & rm"))]));
        let ctx = Ctx {
            vars: &vars,
            outputs: &outputs,
            step_ids: &ids,
            builtins: BTreeMap::new(),
        };
        let e = render_checked("echo {{steps.gen.output}}", &ctx, &|_, v| {
            if v.contains('&') {
                Err("metachar".to_string())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert!(e.contains("metachar"));
        // The author's own text is never checked, only substitutions.
        assert!(render_checked("echo a & b", &ctx, &|_, v| {
            if v.contains('&') {
                Err("metachar".to_string())
            } else {
                Ok(())
            }
        })
        .is_ok());
    }
}
