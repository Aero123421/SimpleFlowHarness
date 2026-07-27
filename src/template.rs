use std::collections::{BTreeMap, HashSet};

/// Latest captured result of a step, exposed to templates.
pub struct StepOutput {
    /// Chain output. For parallel/foreach steps this is the aggregated output.
    pub output: String,
    /// Aggregated output ("--- id ---" separated). Equals `output` for plain steps.
    pub outputs: String,
    pub output_file: String,
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
///   run_dir, flow_dir, step_id, visit, os, prompt_file, notes, item, item_index
/// Filters:
///   head:N (first N lines) | tail:N (last N lines) | truncate:N (first N chars)
///   lines:A-B (1-indexed inclusive) | trim
/// Referencing a step that exists but has not run yet yields an empty string.
pub fn render(input: &str, ctx: &Ctx) -> Result<String, String> {
    render_impl(input, ctx, None)
}

/// Like render, but every substituted value is passed to `check(key, value)`
/// before insertion. Used for shell-string cmd: to reject values that would be
/// re-parsed by cmd.exe / sh.
pub fn render_checked(
    input: &str,
    ctx: &Ctx,
    check: &dyn Fn(&str, &str) -> Result<(), String>,
) -> Result<String, String> {
    render_impl(input, ctx, Some(check))
}

fn render_impl(
    input: &str,
    ctx: &Ctx,
    check: Option<&dyn Fn(&str, &str) -> Result<(), String>>,
) -> Result<String, String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
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
        return ctx
            .vars
            .get(name)
            .cloned()
            .ok_or_else(|| format!("undefined variable '{name}' (define it under vars: or pass --var {name}=...)"));
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
            other => Err(format!(
                "unknown field 'steps.{id}.{other}' (use output, outputs or output_file)"
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
                Ok(format!("{cut}\n...[truncated {} of {total} chars]", total - n))
            }
        }
        "lines" => {
            let a = arg.ok_or("filter 'lines' needs A-B")?;
            let (s, e) = a
                .split_once('-')
                .ok_or("filter 'lines' needs A-B (1-indexed, inclusive)")?;
            let s: usize = s.trim().parse().map_err(|_| "lines: bad start".to_string())?;
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
        other => Err(format!("unknown filter '{other}' (head/tail/truncate/lines/trim)")),
    }
}
