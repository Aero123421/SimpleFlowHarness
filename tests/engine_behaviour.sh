#!/usr/bin/env bash
# Engine behaviour tests that need a real process tree but no AI calls.
# Usage: bash tests/engine_behaviour.sh ./target/release/sfh
set -uo pipefail

SFH="${1:-./target/release/sfh}"
SFH="$(cd "$(dirname "$SFH")" && pwd)/$(basename "$SFH")"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"

pass=0
fail=0
check() { # check <name> <expected-exit> <actual-exit> [extra condition message]
  if [ "$2" = "$3" ]; then
    echo "ok   - $1"
    pass=$((pass + 1))
  else
    echo "FAIL - $1 (expected exit $2, got $3)"
    fail=$((fail + 1))
  fi
}
contains() { # contains <name> <needle> <file>
  if grep -qF -e "$2" "$3"; then
    echo "ok   - $1"
    pass=$((pass + 1))
  else
    echo "FAIL - $1 (missing '$2' in $3)"
    sed -n '1,40p' "$3"
    fail=$((fail + 1))
  fi
}

# --- routing, foreach fan-out and filters ------------------------------------
cat > basic.yaml <<'YAML'
name: basic
vars:
  items: |
    alpha
    beta
steps:
  - id: fan
    max_parallel: 2
    parallel:
      - id: a
        cmd: ["echo", "AAA"]
      - id: b
        cmd: ["echo", "BBB"]
    route:
      - when_contains: "BBB"
        goto: each
      - goto: fail
  - id: each
    foreach: { from: "{{vars.items}}" }
    cmd: ["echo", "item={{item}}/{{item_index}}"]
  - id: last
    cmd: ["echo", "done:{{steps.a.output | trim}}"]
YAML
"$SFH" run basic.yaml -q > basic.out 2> basic.err
check "parallel+foreach+filters run" 0 $?
contains "child output is addressable" "done:AAA" basic.out

# --- verdict trailer routing --------------------------------------------------
cat > verdict.yaml <<'YAML'
name: verdict
steps:
  - id: gen
    cmd: ["printf", "discussing VERDICT: REVISE in prose\nVERDICT: OK\n"]
    route:
      - when_last_line_contains: "VERDICT: OK"
        goto: good
      - goto: fail
  - id: good
    cmd: ["echo", "routed-on-last-line"]
YAML
"$SFH" run verdict.yaml -q > verdict.out 2>&1
check "last-line routing ignores prose mentions" 0 $?
contains "took the OK branch" "routed-on-last-line" verdict.out

# --- max_visits degradation ---------------------------------------------------
cat > visits.yaml <<'YAML'
name: visits
steps:
  - id: loop
    cmd: ["echo", "again"]
    max_visits: 2
    on_max_visits: goto:after
    route:
      - goto: loop
  - id: after
    cmd: ["echo", "degraded-gracefully"]
YAML
"$SFH" run visits.yaml -q > visits.out 2>&1
check "on_max_visits degrades instead of failing the flow" 0 $?
contains "reached the fallback step" "degraded-gracefully" visits.out

# --- empty output from a cmd step is allowed, shell injection is not ----------
cat > guard.yaml <<'YAML'
name: guard
vars:
  evil: "hello & echo pwned"
steps:
  - id: t
    cmd: "echo {{vars.evil}}"
YAML
"$SFH" run guard.yaml -q > guard.out 2> guard.err
check "shell metacharacters in substitutions are rejected" 1 $?
contains "explains why" "metacharacters" guard.err

# --- failure: partial emit + resume ------------------------------------------
cat > resume.yaml <<'YAML'
name: resume
steps:
  - id: one
    cmd: ["echo", "one-output"]
  - id: boom
    cmd: ["sh", "-c", "exit 7"]
  - id: after
    cmd: ["echo", "after:{{steps.one.output | trim}}"]
YAML
"$SFH" run resume.yaml -q > r1.out 2> r1.err
check "flow fails on a failing step" 1 $?
contains "still emits the last good output" "one-output" r1.out
contains "tells the user how to resume" "--resume" r1.err

cat > resume.yaml <<'YAML'
name: resume
steps:
  - id: one
    cmd: ["echo", "one-output"]
  - id: boom
    cmd: ["echo", "fixed"]
  - id: after
    cmd: ["echo", "after:{{steps.one.output | trim}}"]
YAML
"$SFH" run resume.yaml --resume-latest > r2.out 2> r2.err
check "resume refuses when the flow changed" 2 $?
contains "names the fingerprint mismatch" "different version" r2.err
"$SFH" run resume.yaml --resume-latest --force-resume > r3.out 2> r3.err
if ! check "forced resume completes" 0 $?; then sed -n '1,20p' r3.err; fi
contains "reused the earlier step output" "after:one-output" r3.out
if grep -q "\[one\] start" r3.err; then
  echo "FAIL - resume re-ran an already completed step"
  fail=$((fail + 1))
else
  echo "ok   - resume skipped completed steps"
  pass=$((pass + 1))
fi

# --- timeout kills the step, and a background grandchild cannot hang the flow --
cat > timeout.yaml <<'YAML'
name: timeout
steps:
  - id: slow
    cmd: ["sh", "-c", "sleep 30 & echo started; sleep 30"]
    timeout_sec: 3
    on_error: continue
  - id: done
    cmd: ["echo", "flow-continued"]
YAML
start=$(date +%s)
"$SFH" run timeout.yaml -q > to.out 2>&1
rc=$?
elapsed=$(( $(date +%s) - start ))
check "timed-out step does not hang the flow" 0 $rc
contains "flow continued after the timeout" "flow-continued" to.out
if [ "$elapsed" -lt 25 ]; then
  echo "ok   - timeout enforced (${elapsed}s)"
  pass=$((pass + 1))
else
  echo "FAIL - timeout not enforced (${elapsed}s)"
  fail=$((fail + 1))
fi

# --- runs subcommands ---------------------------------------------------------
"$SFH" runs list > runs.out 2>&1
check "runs list works" 0 $?
"$SFH" runs clean --older-than 3650 --keep 1 --dry-run > clean.out 2>&1
check "runs clean --dry-run works" 0 $?

echo
echo "engine behaviour: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
