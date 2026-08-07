//! The execution closure: everything outside the flow file that decides what a
//! run actually does.
//!
//! `--resume` has always compared the flow file, which catches the obvious
//! case. It does not catch the rest of the inputs: a profile overlay that
//! swapped the model, a `TASK.md` the flow reads as context, a CLI that
//! upgraded itself between the crash and the resume. Each of those changes what
//! the run means while leaving the flow byte-identical, so the resume silently
//! continues a different piece of work.
//!
//! The closure is a canonical JSON document of all of it, hashed once at run
//! start and compared on resume. A mismatch is refused by default and lists
//! exactly which entries moved, so `--force-resume` is a decision rather than
//! a shrug.

use crate::sha256;
use serde_json::{json, Map, Value};
use std::path::Path;

pub const ALGO: &str = "sha256-canonical-json";
pub const FILE: &str = "execution-closure.json";

/// The inputs a run is pinned to.
#[derive(Default, Clone)]
pub struct Closure {
    entries: Map<String, Value>,
}

impl Closure {
    pub fn new() -> Closure {
        Closure::default()
    }

    pub fn set(&mut self, key: &str, value: Value) -> &mut Closure {
        self.entries.insert(key.to_string(), value);
        self
    }

    /// Pin a file by its bytes and NOTHING else.
    ///
    /// The path is deliberately not part of the entry. What the run did depends
    /// on the bytes it read, not on where they happened to live: the same flow
    /// resumed through a relative path instead of an absolute one, or from a
    /// repository checked out somewhere else, is the same run. Recording the
    /// path made both of those look like a changed input and refused a resume
    /// that was in fact identical. The entry KEY (`flow`, `context.task`) is
    /// what tells a reader which file this was.
    ///
    /// A file that cannot be read is recorded as unreadable, which IS a
    /// difference worth catching: a context file that has since been deleted
    /// must never compare equal to one that is still there. Only the error
    /// KIND is stored - an OS message can embed the path, which would put the
    /// path back into the comparison by the back door.
    pub fn set_file(&mut self, key: &str, path: &Path) -> &mut Closure {
        let value = match std::fs::read(path) {
            Ok(bytes) => {
                let normalized = normalize(&bytes);
                json!({
                    "sha256": sha256::hex(&normalized),
                    "bytes": normalized.len(),
                })
            }
            Err(e) => json!({ "unreadable": format!("{:?}", e.kind()) }),
        };
        self.set(key, value)
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.get(key)
    }

    /// Canonical JSON: keys sorted, no insignificant whitespace, so two runs
    /// that pinned the same inputs produce the same bytes regardless of the
    /// order in which they were recorded.
    pub fn canonical(&self) -> String {
        canonical_json(&Value::Object(self.entries.clone()))
    }

    pub fn fingerprint(&self) -> String {
        sha256::hex(self.canonical().as_bytes())
    }

    pub fn to_json(&self) -> Value {
        json!({
            "schema_version": 1,
            "algo": ALGO,
            "fingerprint": self.fingerprint(),
            "entries": Value::Object(self.entries.clone()),
        })
    }

    pub fn from_json(v: &Value) -> Option<Closure> {
        Some(Closure {
            entries: v.get("entries")?.as_object()?.clone(),
        })
    }

    /// Which entries differ, in a form a human can act on. Empty means the two
    /// runs were pinned to the same inputs.
    pub fn diff(&self, other: &Closure) -> Vec<String> {
        let mut out = Vec::new();
        let mut keys: Vec<&String> = self.entries.keys().chain(other.entries.keys()).collect();
        keys.sort_unstable();
        keys.dedup();
        for k in keys {
            match (self.entries.get(k), other.entries.get(k)) {
                (Some(a), Some(b)) if a != b => {
                    out.push(format!("{k}: {} -> {}", summarize(a), summarize(b)))
                }
                (Some(a), None) => out.push(format!("{k}: {} -> (absent)", summarize(a))),
                (None, Some(b)) => out.push(format!("{k}: (absent) -> {}", summarize(b))),
                _ => {}
            }
        }
        out
    }
}

/// CRLF is folded to LF before hashing, exactly as the flow fingerprint has
/// always done.
///
/// A flow checked out on Windows and the same flow checked out on Linux are the
/// same flow, and a run dir has to survive a re-checkout under a different
/// `core.autocrlf`. Hashing raw bytes here would have re-introduced, for every
/// context file and profile overlay, precisely the bug the flow fingerprint
/// already fixed for the flow itself. Binary content is unaffected in practice:
/// a lone CR is left alone, and only the two-byte CRLF sequence folds.
fn normalize(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            out.push(b'\n');
            i += 2;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// A closure entry as one short line. A file becomes its digest prefix rather
/// than its whole record, because a diff a human cannot read is one they will
/// force past without looking.
fn summarize(v: &Value) -> String {
    if let Some(hash) = v.get("sha256").and_then(|x| x.as_str()) {
        return format!("sha256:{}", &hash[..hash.len().min(12)]);
    }
    if let Some(e) = v.get("unreadable").and_then(|x| x.as_str()) {
        return format!("unreadable ({e})");
    }
    match v {
        Value::String(s) => s.clone(),
        other => {
            let s = other.to_string();
            if s.len() > 60 {
                format!("{}...", &s[..57])
            } else {
                s
            }
        }
    }
}

/// Deterministic serialization. `serde_json` already writes object keys in
/// insertion order for a `Map`, so this walks the value and rebuilds every
/// object with sorted keys before printing.
pub fn canonical_json(v: &Value) -> String {
    fn sorted(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut out = Map::new();
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort_unstable();
                for k in keys {
                    out.insert(k.clone(), sorted(&m[k]));
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_string(&sorted(v)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_does_not_depend_on_the_order_things_were_pinned_in() {
        let mut a = Closure::new();
        a.set("sfh_version", json!("1.2.0"))
            .set("tool.codex.version", json!("0.146.0"))
            .set("workspace.mode", json!("git-worktree"));
        let mut b = Closure::new();
        b.set("workspace.mode", json!("git-worktree"))
            .set("sfh_version", json!("1.2.0"))
            .set("tool.codex.version", json!("0.146.0"));
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert!(a.diff(&b).is_empty());
        assert_eq!(a.fingerprint().len(), 64);
    }

    #[test]
    fn a_changed_input_is_named_rather_than_just_refused() {
        let mut before = Closure::new();
        before
            .set("tool.codex.version", json!("codex 0.146.0"))
            .set("context.task", json!({"sha256": "aaaa1111"}));
        let mut after = Closure::new();
        after
            .set("tool.codex.version", json!("codex 0.147.0"))
            .set("context.task", json!({"sha256": "bbbb2222"}));
        assert_ne!(before.fingerprint(), after.fingerprint());
        let d = before.diff(&after);
        assert_eq!(d.len(), 2, "{d:?}");
        assert!(
            d.iter().any(|l| l.starts_with("tool.codex.version:")),
            "{d:?}"
        );
        assert!(
            d.iter()
                .any(|l| l.contains("sha256:aaaa1111 -> sha256:bbbb2222")),
            "a file change shows both digests: {d:?}"
        );
    }

    /// The property that made a correct resume fail: a closure must depend on
    /// what the run READ, not on the path it read it through.
    #[test]
    fn the_same_bytes_pin_the_same_closure_from_any_path() {
        let dir = std::env::temp_dir().join(format!("sfh-closure-path-{}", std::process::id()));
        let a = dir.join("a");
        let b = dir.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("flow.yaml"), "name: x\n").unwrap();
        std::fs::write(b.join("flow.yaml"), "name: x\n").unwrap();
        let mut one = Closure::new();
        one.set_file("flow", &a.join("flow.yaml"));
        let mut two = Closure::new();
        two.set_file("flow", &b.join("flow.yaml"));
        assert_eq!(one.fingerprint(), two.fingerprint());
        assert!(one.diff(&two).is_empty());
        // The same flow checked out with CRLF is the same flow - the policy the
        // flow fingerprint has always applied, now applied to every pinned file.
        std::fs::write(b.join("flow.yaml"), "name: x\r\n").unwrap();
        let mut crlf = Closure::new();
        crlf.set_file("flow", &b.join("flow.yaml"));
        assert_eq!(
            one.fingerprint(),
            crlf.fingerprint(),
            "a CRLF checkout must not read as a changed input"
        );
        // Different bytes at the same path still differ, which is the point.
        std::fs::write(b.join("flow.yaml"), "name: y\n").unwrap();
        let mut three = Closure::new();
        three.set_file("flow", &b.join("flow.yaml"));
        assert_ne!(one.fingerprint(), three.fingerprint());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_deleted_input_is_a_difference_not_an_equality() {
        // A context file that has since been removed must never compare equal
        // to one that is present: the run read something the resume cannot.
        let dir = std::env::temp_dir().join(format!("sfh-closure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("TASK.md");
        std::fs::write(&f, "do the thing").unwrap();
        let mut before = Closure::new();
        before.set_file("context.task", &f);
        std::fs::remove_file(&f).unwrap();
        let mut after = Closure::new();
        after.set_file("context.task", &f);
        assert_ne!(before.fingerprint(), after.fingerprint());
        assert!(after.diff(&before).iter().any(|l| l.contains("unreadable")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_closure_round_trips_through_its_own_file_format() {
        let mut c = Closure::new();
        c.set("sfh_version", json!("1.2.0")).set(
            "unsafe_overrides",
            json!(["workspace.allow_concurrent_writers"]),
        );
        let back = Closure::from_json(&c.to_json()).expect("round trip");
        assert_eq!(back.fingerprint(), c.fingerprint());
        assert!(back.diff(&c).is_empty());
    }
}
