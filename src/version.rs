use std::cmp::Ordering;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Version {
    core: Vec<u64>,
    pre: Vec<Identifier>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Identifier {
    Number(u64),
    Text(String),
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let width = self.core.len().max(other.core.len());
        for index in 0..width {
            let ordering = self
                .core
                .get(index)
                .copied()
                .unwrap_or(0)
                .cmp(&other.core.get(index).copied().unwrap_or(0));
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            _ => {
                for (left, right) in self.pre.iter().zip(&other.pre) {
                    let ordering = match (left, right) {
                        (Identifier::Number(a), Identifier::Number(b)) => a.cmp(b),
                        (Identifier::Number(_), Identifier::Text(_)) => Ordering::Less,
                        (Identifier::Text(_), Identifier::Number(_)) => Ordering::Greater,
                        (Identifier::Text(a), Identifier::Text(b)) => a.cmp(b),
                    };
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                }
                self.pre.len().cmp(&other.pre.len())
            }
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug)]
enum Operator {
    Eq,
    Gt,
    Ge,
    Lt,
    Le,
}

fn parse_version(text: &str) -> Option<Version> {
    let text = text.trim().strip_prefix('v').unwrap_or(text.trim());
    let without_build = text.split_once('+').map(|(v, _)| v).unwrap_or(text);
    let (core, pre) = without_build
        .split_once('-')
        .map(|(core, pre)| (core, Some(pre)))
        .unwrap_or((without_build, None));
    let core: Vec<u64> = core
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    // A single integer in arbitrary CLI prose is much more likely to be a
    // count or a year than a version. sfh's supported adapters all report at
    // least major.minor, which makes extraction deterministic and fail-closed.
    if core.len() < 2 {
        return None;
    }
    let pre = match pre {
        None => Vec::new(),
        Some("") => return None,
        Some(pre) => pre
            .split('.')
            .map(|part| {
                if part.is_empty() || !part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    return None;
                }
                Some(match part.parse::<u64>() {
                    Ok(value) => Identifier::Number(value),
                    Err(_) => Identifier::Text(part.to_ascii_lowercase()),
                })
            })
            .collect::<Option<Vec<_>>>()?,
    };
    Some(Version { core, pre })
}

fn parse_requirement(requirement: &str) -> Result<Vec<(Operator, Version)>, String> {
    let mut clauses = Vec::new();
    for raw in requirement.split(',') {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("contains an empty comma-separated clause".into());
        }
        let (operator, value) = if let Some(value) = raw.strip_prefix(">=") {
            (Operator::Ge, value)
        } else if let Some(value) = raw.strip_prefix("<=") {
            (Operator::Le, value)
        } else if let Some(value) = raw.strip_prefix("==") {
            (Operator::Eq, value)
        } else if let Some(value) = raw.strip_prefix('>') {
            (Operator::Gt, value)
        } else if let Some(value) = raw.strip_prefix('<') {
            (Operator::Lt, value)
        } else if let Some(value) = raw.strip_prefix('=') {
            (Operator::Eq, value)
        } else {
            (Operator::Eq, raw)
        };
        let version = parse_version(value.trim()).ok_or_else(|| {
            format!(
                "clause '{raw}' needs a numeric major.minor version, optionally with patch/prerelease/build parts"
            )
        })?;
        clauses.push((operator, version));
    }
    if clauses.is_empty() {
        return Err("must not be empty".into());
    }
    Ok(clauses)
}

pub fn validate_requirement(requirement: &str) -> Result<(), String> {
    parse_requirement(requirement).map(|_| ())
}

fn extract_version(output: &str) -> Option<Version> {
    output
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+'))
        .filter(|candidate| candidate.chars().any(|c| c.is_ascii_digit()))
        .find_map(parse_version)
}

/// Check a comma-separated exact/range declaration against one CLI's raw
/// `--version` line. Requirement syntax is validated when the flow is loaded;
/// this still returns a Result so callers fail closed on unusable probe text.
pub fn satisfies(requirement: &str, observed: &str) -> Result<bool, String> {
    let clauses = parse_requirement(requirement)?;
    let observed = extract_version(observed).ok_or_else(|| {
        format!("could not find a numeric major.minor version in --version output {observed:?}")
    })?;
    Ok(clauses.into_iter().all(|(operator, required)| {
        let ordering = observed.cmp(&required);
        match operator {
            Operator::Eq => ordering == Ordering::Equal,
            Operator::Gt => ordering == Ordering::Greater,
            Operator::Ge => ordering != Ordering::Less,
            Operator::Lt => ordering == Ordering::Less,
            Operator::Le => ordering != Ordering::Greater,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_comma_separated_ranges_are_supported() {
        assert!(satisfies("1.2.3", "tool 1.2.3").unwrap());
        assert!(satisfies(">=1.2, <2.0", "tool version v1.9.4+build.7").unwrap());
        assert!(!satisfies(">=1.2, <2.0", "tool 2.0.0").unwrap());
    }

    #[test]
    fn prerelease_order_follows_semver_rules() {
        assert!(satisfies(">=1.2.0-beta.2", "tool 1.2.0-beta.11").unwrap());
        assert!(satisfies(">1.2.0-rc.1", "tool 1.2.0").unwrap());
        assert!(!satisfies(">=1.2.0", "tool 1.2.0-rc.1").unwrap());
    }

    #[test]
    fn invalid_requirements_and_unusable_probe_text_fail_closed() {
        assert!(validate_requirement(">=1").is_err());
        assert!(validate_requirement(">=1.2,").is_err());
        assert!(satisfies(">=1.2", "tool build unknown").is_err());
    }
}
