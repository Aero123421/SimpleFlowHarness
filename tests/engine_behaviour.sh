#!/usr/bin/env bash
# Engine behaviour tests that need a real process tree but no AI calls.
# Usage: bash tests/engine_behaviour.sh ./target/release/sfh
set -uo pipefail

SFH="${1:-./target/release/sfh}"
SFH="$(cd "$(dirname "$SFH")" && pwd)/$(basename "$SFH")"
# Resolved before the cd below, because the session stub's source lives next to
# this script and everything after the cd is relative to a temp dir.
SUITE_DIR="$(cd "$(dirname "$0")" && pwd)"
WORK="$(mktemp -d)"
# Windows holds a directory busy while any process still has a handle inside
# it, and this suite deliberately starts DETACHED runs that outlive it - that
# is the feature under test, not a leak. A cleanup that loses the race used to
# turn an all-green run red, because a failing command in an EXIT trap replaces
# the script's exit status. Retry briefly, then give up quietly, and always
# hand back the status the tests actually produced.
cleanup() {
  rc=$?
  for _ in 1 2 3 4 5; do
    rm -rf "$WORK" 2>/dev/null && break
    sleep 1
  done
  exit "$rc"
}
trap cleanup EXIT
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
not_contains() { # not_contains <name> <needle> <file>
  if grep -qF -e "$2" "$3"; then
    echo "FAIL - $1 (unexpected '$2' in $3)"
    sed -n '1,40p' "$3"
    fail=$((fail + 1))
  else
    echo "ok   - $1"
    pass=$((pass + 1))
  fi
}
# Real (native) symlink support. msys' default ln -s writes a copy or a marker
# file a native Windows binary does not follow, which would test nothing; force
# native-or-fail so the symlink attacks are only run where they can exist.
have_symlinks() {
  rm -f "$WORK/.sym-probe"
  MSYS=winsymlinks:nativestrict ln -s /nonexistent-sfh-target "$WORK/.sym-probe" 2>/dev/null || return 1
  [ -L "$WORK/.sym-probe" ]
}
# The Windows pid of a background job. msys' bash reports its OWN internal pid
# in $!, which a native Windows binary (sfh) cannot see; the real one is in
# /proc/<pid>/winpid. On real Unix the pid is already the OS pid.
winpid_of() {
  cat "/proc/$1/winpid" 2>/dev/null || echo "$1"
}
# FNV-1a 64 over a file's bytes as 16 lowercase hex chars - the flow fingerprint
# sfh <= 0.9 recorded in meta.json. Bash int64 arithmetic wraps mod 2^64 with
# the same two's-complement bits the Rust implementation uses.
fnv1a64() {
  local h=-3750763034362895579 # 0xcbf29ce484222325
  local b
  for b in $(od -An -tu1 -v "$1"); do
    h=$(( (h ^ b) * 0x100000001b3 ))
  done
  printf '%016x' "$h"
}

# --- session-reporting stub CLI (T-0 / B-15) ---------------------------------
# `bin: "echo"` cannot report a session id, so every test that stands echo in
# for claude fails F-11's "resume unverified" check and can only prove that a
# guard did NOT fire. tests/stub/session_stub.rs speaks the shape sfh parses
# (`claude -p --output-format json`: one envelope with .result/.session_id/
# .usage), so a session can be opened and continued for real without calling an
# AI. Built once here, into this suite's own temp dir, with the rustc that built
# sfh - a missing toolchain is a loud failure, not a silent skip.
STUB_NAME="sfh-session-stub"
case "$(uname 2>/dev/null)" in
  MINGW*|MSYS*|CYGWIN*) STUB_NAME="sfh-session-stub.exe" ;;
esac
STUB="$WORK/$STUB_NAME"
if rustc -O --edition 2021 -o "$STUB" "$SUITE_DIR/stub/session_stub.rs" > stub-build.log 2>&1; then
  echo "ok   - the session stub builds"
  pass=$((pass + 1))
else
  echo "FAIL - the session stub did not build (needs rustc on PATH)"
  sed -n '1,40p' stub-build.log
  fail=$((fail + 1))
fi
# What a flow's bin: gets. sfh is a native binary, so under msys it cannot
# resolve the /tmp/... path bash sees; hand it the drive-letter form. Absolute
# either way, so a step with its own cwd: still finds it.
if command -v cygpath > /dev/null 2>&1; then
  STUB_BIN="$(cygpath -m "$STUB")"
else
  STUB_BIN="$STUB"
fi

# Smoke test: the stub answers in the one shape sfh parses, echoes the session
# id sfh assigned, and keeps its knobs independent of each other.
"$STUB" -p --output-format json --permission-mode dontAsk --tools "Read,Grep" \
  --session-id stub-smoke-1 --stub-last-line VERDICT-OK --stub-quote VERDICT-OK \
  < /dev/null > stub-smoke.json 2> stub-smoke.err
check "the stub exits 0 and ignores the preset's own flags" 0 $?
STUB_SMOKE_LINES="$(awk 'END { print NR }' stub-smoke.json)"
check "the stub answers on exactly one line" 1 "$STUB_SMOKE_LINES"
contains "the stub echoes the session id sfh assigned" '"session_id":"stub-smoke-1"' stub-smoke.json
contains "the stub reports usage" '"usage":{"input_tokens":11,"output_tokens":7}' stub-smoke.json
contains "the stub can quote a needle inside the body" 'VERDICT-OK\nsfh-stub: the line above was quoted' stub-smoke.json
contains "the quoted needle is not the last line" '\nVERDICT-OK","session_id"' stub-smoke.json
# A fork gets -r <parent> AND --session-id <child>; sfh fails the step when the
# child id comes back as the parent, so the child has to win.
"$STUB" -p --output-format json -r stub-parent --fork-session --session-id stub-child \
  < /dev/null > stub-fork.json 2>&1
contains "a fork reports the child session, not the parent" '"session_id":"stub-child"' stub-fork.json
# Exit code and verdict text are independent: "said the right thing, still
# failed" is the case a member-vote count has to refuse to count.
SFH_STUB_LAST_LINE=VERDICT-OK "$STUB" --stub-exit 1 < /dev/null > stub-exit.json 2>&1
check "the stub takes its exit code and last line from the environment" 1 $?
contains "a failing stub still reports the requested last line" '\nVERDICT-OK","session_id"' stub-exit.json
"$STUB" --stub-plain --stub-stderr-every 20 --stub-sleep 0.2 --stub-last-line PLAIN-OK \
  < /dev/null > stub-plain.out 2> stub-plain.err
check "the stub runs as a plain cmd: leaf" 0 $?
contains "plain mode prints the body, not JSON" "PLAIN-OK" stub-plain.out
contains "progress goes to stderr only" "sfh-stub: progress 1" stub-plain.err
if grep -q "progress" stub-plain.out; then
  echo "FAIL - stderr progress leaked into stdout"
  fail=$((fail + 1))
else
  echo "ok   - stderr progress stays out of stdout"
  pass=$((pass + 1))
fi

# The payoff (B-15): a session opened by the stub can actually be continued, and
# sfh verifies the continuation instead of merely failing to object.
cat > stub-session.yaml <<YAML
name: stub-session
steps:
  - id: first
    tool: claude
    bin: "$STUB_BIN"
    access: read
    prompt: "open a session"
  - id: second
    tool: claude
    bin: "$STUB_BIN"
    access: read
    continue_from: first
    prompt: "continue it"
YAML
"$SFH" run stub-session.yaml --runs-dir stub-runs -q > stub-session.out 2> stub-session.err
check "a stub session can be opened and continued" 0 $?
STUB_LOG="$(find stub-runs -type f -name 'log.jsonl' -print -quit)"
STUB_SESSIONS="$(grep -cF '"session":{"access":"read"' "$STUB_LOG")"
check "both steps record the session they ran under" 2 "$STUB_SESSIONS"
if grep -q "resume unverified" stub-session.err; then
  echo "FAIL - the continuation was not verifiable against a reported session id"
  fail=$((fail + 1))
else
  echo "ok   - the continuation was verified against the reported session id"
  pass=$((pass + 1))
fi
# Negative control. The same flow with the old stand-in must still fail, or the
# check above proves nothing about the stub: echo reports no session id, so
# F-11 cannot tell a resume from a fresh session and refuses.
sed "s#$STUB_BIN#echo#g" stub-session.yaml > stub-session-echo.yaml
"$SFH" run stub-session-echo.yaml -q > stub-session-echo.out 2> stub-session-echo.err
check "the same flow with bin: echo still cannot verify the resume" 1 $?
contains "echo fails for the session reason, not another one" "resume unverified" stub-session-echo.err

# --- built-in AI guide -------------------------------------------------------
"$SFH" guide > guide.out 2> guide.err
check "guide prints without arguments" 0 $?
GUIDE_LINES="$(awk 'END { print NR }' guide.out)"
if [ "$GUIDE_LINES" -le 80 ]; then
  echo "ok   - guide stays within 80 lines"
  pass=$((pass + 1))
else
  echo "FAIL - guide is $GUIDE_LINES lines (maximum 80)"
  fail=$((fail + 1))
fi
"$SFH" guide unexpected > guide-args.out 2>&1
check "guide rejects arguments" 2 $?

# --- CLI help and option contracts -------------------------------------------
"$SFH" --help > cli-help.out 2> cli-help.err
check "top-level help succeeds" 0 $?
contains "top-level help discovers prompt_file" "{{prompt_file}}" cli-help.out
contains "top-level help discovers command-specific help" "sfh help [command]" cli-help.out
"$SFH" help run > run-help.out 2> run-help.err
check "help <command> succeeds" 0 $?
contains "run help discovers the deterministic run directory" "--run-dir" run-help.out
contains "run help discovers rendered prompt files" "{{prompt_file}}" run-help.out
"$SFH" help run ignored > help-extra.out 2> help-extra.err
check "help rejects silently ignored extra arguments" 2 $?
contains "help explains its accepted shape" "usage: sfh help [command]" help-extra.err
"$SFH" runs list --help > runs-list-help.out 2> runs-list-help.err
check "nested runs help keeps its specific usage" 0 $?
contains "runs list help names its limit option" "-n N" runs-list-help.out

"$SFH" plan missing-flow.yaml -q > plan-quiet.out 2> plan-quiet.err
check "plan rejects a quiet flag it does not implement" 2 $?
contains "plan's error is a normal unknown flag" "unknown flag '-q'" plan-quiet.err
"$SFH" plan missing-flow.yaml -v > plan-verbose.out 2> plan-verbose.err
check "plan rejects a verbose flag it does not implement" 2 $?
contains "verbose is not mislabeled as a run-only concept" "unknown flag '-v'" plan-verbose.err
"$SFH" status -q > status-quiet.out 2> status-quiet.err
check "status rejects a quiet flag it does not implement" 2 $?
contains "status explains the rejected flag" "unknown flag '-q'" status-quiet.err
"$SFH" stop --quiet > stop-quiet.out 2> stop-quiet.err
check "stop rejects a quiet flag it does not implement" 2 $?
contains "stop explains the rejected flag" "unknown flag '--quiet'" stop-quiet.err

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
"$SFH" run basic.yaml --runs-dir basic-runs -q > basic.out 2> basic.err
check "parallel+foreach+filters run" 0 $?
contains "child output is addressable" "done:AAA" basic.out
BASIC_LOG="$(find basic-runs -type f -name 'log.jsonl' -print -quit)"
contains "a matched route records via=rule" '"via":"rule"' "$BASIC_LOG"
contains "an unmatched route records via=fallthrough" '"via":"fallthrough"' "$BASIC_LOG"
contains "step_end records an output hash" '"output_hash":"cb1ad2119d8fafb69566510ee712661f9f14b83385006ef92aec47f523a38358"' "$BASIC_LOG"
if grep -F '"event":"aggregate_end"' "$BASIC_LOG" |
  grep -F '"step":"fan"' |
  grep -qF '"output_hash":"68d478004ba12d2dbe1ee1766b705d68c1b7fd5d1c31132412c0fba38407f8e8"'; then
  echo "ok   - aggregate hash is computed from the unlabeled plain output"
  pass=$((pass + 1))
else
  echo "FAIL - aggregate hash did not match plain output AAA\\\\n\\\\nBBB"
  fail=$((fail + 1))
fi

# JSON fan-out accepts AI prose but deterministically uses the final complete
# array. A citation or draft array before it must not be glued to the answer.
cat > foreach-json.yaml <<'YAML'
api_version: 1
name: foreach-json
vars:
  payload: |
    See source [1].
    Draft: ["old"]
    Final: ["alpha", ["beta", 2], {"ok": true}]
steps:
  - id: each
    foreach: {from: "{{vars.payload}}", split: json}
    cmd: ["sh", "-c", "printf '%s\n' \"$1\"", "foreach-json", "{{item}}"]
YAML
"$SFH" run foreach-json.yaml -q > foreach-json.out 2> foreach-json.err
check "foreach split=json accepts prose around a final array" 0 $?
contains "foreach keeps scalar items" "alpha" foreach-json.out
contains "foreach keeps nested arrays intact" '["beta",2]' foreach-json.out
contains "foreach keeps object items intact" '{"ok":true}' foreach-json.out
not_contains "foreach ignores an earlier draft array" "old" foreach-json.out

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

# --- portable string cmd validation ------------------------------------------
cat > shell-sh-only.yaml <<'YAML'
steps:
  - id: check
    cmd: 'cargo test; echo "$?"'
YAML
"$SFH" validate shell-sh-only.yaml > shell-sh-only.out 2>&1
check "sh-only syntax in a string cmd is rejected on every OS" 2 $?
contains "the sh-only error identifies the syntax" '`$?`' shell-sh-only.out
contains "the sh-only error explains the array-form fix" 'cmd: ["sh", "-c", "..."]' shell-sh-only.out

cat > shell-cmd-only.yaml <<'YAML'
steps:
  - id: check
    cmd: 'echo %RESULT% ^> result.txt'
YAML
"$SFH" validate shell-cmd-only.yaml > shell-cmd-only.out 2>&1
check "cmd-only syntax in a string cmd is rejected on every OS" 2 $?
contains "the cmd-only error identifies the syntax" '`%NAME%`' shell-cmd-only.out
contains "the cmd-only error explains the array-form fix" 'cmd: ["cmd", "/C", "..."]' shell-cmd-only.out

cat > shell-portable.yaml <<'YAML'
steps:
  - id: check
    cmd: 'echo "quoted;semicolon" && echo ''also;quoted'' | echo ok > result.txt 2>&1'
YAML
"$SFH" validate shell-portable.yaml > shell-portable.out 2>&1
check "portable operators and quoted semicolons are accepted" 0 $?

# --- consecutive branch fallthrough warning ---------------------------------
cat > branch-fallthrough.yaml <<'YAML'
steps:
  - id: choose
    cmd: ["echo", "MET"]
    route:
      - {when_last_line_is: "MET", goto: met}
      - {when_last_line_is: "UNMET", goto: unmet}
      - {when_last_line_is: "UNCLEAR", goto: unclear}
  - id: met
    cmd: ["echo", "met"]
  - id: unmet
    cmd: ["echo", "unmet"]
  - id: unclear
    cmd: ["echo", "unclear"]
YAML
"$SFH" validate branch-fallthrough.yaml > branch-fallthrough.out 2>&1
check "unterminated consecutive branches are a warning, not an error" 0 $?
contains "the first leaking branch is named" "step 'met'" branch-fallthrough.out
contains "the next branch it leaks into is named" "step 'unmet'" branch-fallthrough.out
"$SFH" run branch-fallthrough.yaml -q > branch-fallthrough-quiet.out 2> branch-fallthrough-quiet.err
check "quiet run with a branch warning still succeeds" 0 $?
not_contains "quiet suppresses runtime branch warnings" "consecutive branch destination" branch-fallthrough-quiet.err
"$SFH" run branch-fallthrough.yaml > branch-fallthrough-run.out 2> branch-fallthrough-run.err
check "non-quiet run with a branch warning still succeeds" 0 $?
contains "non-quiet run explains the suspicious fallthrough" "consecutive branch destination" branch-fallthrough-run.err

# --- overlapping contains verdicts are rejected ------------------------------
cat > overlapping-verdicts.yaml <<'YAML'
steps:
  - id: choose
    cmd: ["echo", "NOT-ACHIEVED"]
    route:
      - {when_last_line_contains: "ACHIEVED", goto: end}
      - {when_last_line_contains: "NOT-ACHIEVED", goto: fail}
YAML
"$SFH" validate overlapping-verdicts.yaml > overlapping-verdicts.out 2>&1
check "overlapping last-line contains phrases are rejected" 2 $?
contains "the overlap error gives the exact-match fix" "when_last_line_is" overlapping-verdicts.out

# --- exact last-line routing -------------------------------------------------
cat > exact-verdict.yaml <<'YAML'
steps:
  - id: choose
    cmd: ["printf", "prose mentions ACHIEVED\n  NOT-ACHIEVED  \n"]
    route:
      - {when_last_line_is: "ACHIEVED", goto: achieved}
      - {when_last_line_is: "NOT-ACHIEVED", goto: not_achieved}
      - {goto: fail}
  - id: achieved
    cmd: ["echo", "wrong-exact-branch"]
    route: [{goto: fail}]
  - id: not_achieved
    cmd: ["echo", "right-exact-branch"]
    route: [{goto: end}]
YAML
"$SFH" run exact-verdict.yaml -q > exact-verdict.out 2>&1
check "when_last_line_is trims and matches only the whole last line" 0 $?
contains "exact matching avoids the substring branch" "right-exact-branch" exact-verdict.out
"$SFH" run exact-verdict.yaml --dry-run > exact-verdict-dry.out 2>&1
check "dry-run accepts when_last_line_is" 0 $?
contains "dry-run displays the exact predicate" 'last line is "NOT-ACHIEVED"' exact-verdict-dry.out

# --- catch-all routing is distinct from a matched predicate ------------------
cat > catch-all.yaml <<'YAML'
name: catch-all
steps:
  - id: choose
    cmd: ["echo", "no predicate matches"]
    route:
      - when_contains: "missing"
        goto: fail
      - goto: done
  - id: done
    cmd: ["echo", "catch-all-ran"]
YAML
"$SFH" run catch-all.yaml --runs-dir catch-all-runs -q > catch-all.out 2>&1
check "a predicate-free route runs as the catch-all" 0 $?
CATCH_LOG="$(find catch-all-runs -type f -name 'log.jsonl' -print -quit)"
contains "catch-all routing records its reason" '"via":"catch_all"' "$CATCH_LOG"

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
"$SFH" run visits.yaml --runs-dir visits-runs -q > visits.out 2>&1
check "on_max_visits degrades instead of failing the flow" 0 $?
contains "reached the fallback step" "degraded-gracefully" visits.out
VISITS_LOG="$(find visits-runs -type f -name 'log.jsonl' -print -quit)"
contains "max_visits routing records its reason" '"via":"max_visits"' "$VISITS_LOG"

# --- max_visits entry order in a two-node loop -------------------------------
cat > visits-first-hook.yaml <<'YAML'
name: visits-first-hook
steps:
  - id: first
    cmd: ["echo", "first"]
    max_visits: 2
    on_max_visits: goto:after
    route:
      - goto: second
  - id: second
    cmd: ["echo", "second"]
    max_visits: 2
    route:
      - goto: first
  - id: after
    cmd: ["echo", "degraded-gracefully"]
YAML
"$SFH" run visits-first-hook.yaml -q > visits-first-hook.out 2>&1
check "the first-entered node reaches an equal visit limit first" 0 $?
contains "the first node's hook reaches the fallback" "degraded-gracefully" visits-first-hook.out

cat > visits-second-hook.yaml <<'YAML'
name: visits-second-hook
steps:
  - id: first
    cmd: ["echo", "first"]
    max_visits: 2
    route:
      - goto: second
  - id: second
    cmd: ["echo", "second"]
    max_visits: 2
    on_max_visits: goto:after
    route:
      - goto: first
  - id: after
    cmd: ["echo", "must-not-reach"]
YAML
"$SFH" run visits-second-hook.yaml -q > visits-second-hook.out 2>&1
check "a hook only on the later-entered node cannot catch the limit" 1 $?
contains "the error identifies the first-entered node" "step 'first' exceeded max_visits" visits-second-hook.out
if grep -qF -e "must-not-reach" visits-second-hook.out; then
  echo "FAIL - the later node's hook incorrectly reached its fallback"
  fail=$((fail + 1))
else
  echo "ok   - the later node's hook did not run"
  pass=$((pass + 1))
fi

# --- compact instruction rendering + pre-compact notes -----------------------
# `echo` stands in for codex: it exits successfully without calling an AI, while
# sfh still writes the exact compact prompt and replaces the chain output.
cat > compact.yaml <<'YAML'
name: compact
vars:
  topic: "rendered-topic"
steps:
  - id: source
    cmd: ["printf", "ORIGINAL-BEFORE-COMPACT-0123456789"]
    notes: append
    compact:
      when_over: 10
      tool: codex
      bin: "echo"
      instruction: "INSTRUCTION={{vars.topic}}"
  - id: after
    cmd: ["echo", "compact-finished"]
YAML
"$SFH" run compact.yaml --runs-dir compact-runs -q > compact.out 2> compact.err
check "compact flow runs with a fake summarizer" 0 $?
COMPACT_PROMPT="$(find compact-runs -type f -name 'source.compact.prompt.txt' -print -quit)"
COMPACT_NOTES="$(find compact-runs -type f -name 'notes.md' -print -quit)"
contains "compact.instruction templates are rendered" "INSTRUCTION=rendered-topic" "$COMPACT_PROMPT"
if grep -qF -e '{{vars.topic}}' "$COMPACT_PROMPT"; then
  echo "FAIL - compact prompt retained the literal instruction template"
  fail=$((fail + 1))
else
  echo "ok   - compact prompt contains no literal instruction template"
  pass=$((pass + 1))
fi
contains "notes preserve the pre-compact original" "ORIGINAL-BEFORE-COMPACT-0123456789" "$COMPACT_NOTES"

# Rewind to the durable source result, before compact/notes reached
# postprocess_end. Resume must run only that post-processing stage: the source
# command is already complete and must not be billed/executed again.
COMPACT_DIR="$(dirname "$COMPACT_PROMPT")"
cp -r "$COMPACT_DIR" compact-checkpoint
awk '{ print } /"event":"step_end"/ && /"step":"source"/ { exit }' \
  "$COMPACT_DIR/log.jsonl" > compact-checkpoint/log.jsonl
cp "$COMPACT_DIR/source.precompact.txt" compact-checkpoint/source.chain.txt
rm -f compact-checkpoint/source.compact.* compact-checkpoint/source.precompact.txt \
  compact-checkpoint/notes.md
"$SFH" run compact.yaml --resume compact-checkpoint -q \
  > compact-resume.out 2> compact-resume.err
check "post-processing resumes without rerunning the completed source" 0 $?
COMPACT_SOURCE_STARTS="$(grep -F '"event":"step_start"' compact-checkpoint/log.jsonl |
  grep -cF '"step":"source"')"
check "the completed source has only its original start" 1 "$COMPACT_SOURCE_STARTS"
contains "the resumed compactor reaches a durable end" \
  '"event":"compact_end"' compact-checkpoint/log.jsonl
contains "compact and notes finish before routing resumes" \
  '"event":"postprocess_end"' compact-checkpoint/log.jsonl
contains "the flow continues after resumed post-processing" \
  "compact-finished" compact-resume.out
contains "resumed notes still use the pre-compact original" \
  "ORIGINAL-BEFORE-COMPACT-0123456789" compact-checkpoint/notes.md

# Rewind one event later: the paid compactor result and notes file are already
# durable, but neither notes_end nor postprocess_end is in the log. Resume must
# trust the compact checkpoint (no second summarizer call), recognize the
# content-derived note marker (no duplicate section), and only finish the two
# cheap log checkpoints before routing.
cp -r "$COMPACT_DIR" compact-substage-checkpoint
awk '{ print } /"event":"compact_end"/ && /"step":"source"/ { exit }' \
  "$COMPACT_DIR/log.jsonl" > compact-substage-checkpoint/log.jsonl
"$SFH" run compact.yaml --resume compact-substage-checkpoint -q \
  > compact-substage-resume.out 2> compact-substage-resume.err
check "a completed compact substage resumes without a second summarizer" 0 $?
COMPACT_SUB_STARTS="$(
  grep -F '"event":"compact_start"' compact-substage-checkpoint/log.jsonl |
    grep -cF '"step":"source"'
)"
check "the compact substage has only its original start" 1 "$COMPACT_SUB_STARTS"
COMPACT_SUB_ENDS="$(
  grep -F '"event":"compact_end"' compact-substage-checkpoint/log.jsonl |
    grep -cF '"step":"source"'
)"
check "the compact substage has only its original end" 1 "$COMPACT_SUB_ENDS"
COMPACT_NOTE_HEADINGS="$(
  grep -cF '## source (visit 1)' compact-substage-checkpoint/notes.md
)"
check "atomic notes recovery does not duplicate a visit section" 1 "$COMPACT_NOTE_HEADINGS"
contains "notes recovery records its durable substage end" \
  '"event":"notes_end"' compact-substage-checkpoint/log.jsonl
contains "routing continues after exact post-processing recovery" \
  "compact-finished" compact-substage-resume.out

# A summarizer that exits non-zero is still a paid attempt. Its reported usage
# must enter the run total before the head+tail fallback continues, otherwise a
# failed compactor can spend past max_cost_usd and the next step still runs.
rm -f compact-failed-cost-next.marker
cat > compact-failed-cost.yaml <<YAML
name: compact-failed-cost
defaults:
  max_cost_usd: 0.15
steps:
  - id: source
    cmd: ["printf", "LONG-OUTPUT-FOR-A-PAID-FAILED-COMPACTOR"]
    compact:
      when_over: 1
      tool: claude
      bin: "$STUB_BIN"
  - id: forbidden
    cmd: ["sh", "-c", "printf 'ran\n' > compact-failed-cost-next.marker"]
YAML
SFH_STUB_COST=0.20 SFH_STUB_EXIT=1 \
  "$SFH" run compact-failed-cost.yaml --runs-dir compact-failed-cost-runs -q \
  > compact-failed-cost.out 2> compact-failed-cost.err
check "a paid failed compactor still trips the cost ceiling" 1 $?
COMPACT_FAILED_COST_LOG="$(
  find compact-failed-cost-runs -type f -name 'log.jsonl' -print -quit
)"
contains "compact_failed records the paid attempt cost" \
  '"cost_usd":0.2' "$COMPACT_FAILED_COST_LOG"
if [ -e compact-failed-cost-next.marker ]; then
  echo "FAIL - failed compactor usage escaped the cost guard"
  fail=$((fail + 1))
else
  echo "ok   - failed compactor usage blocks the following step"
  pass=$((pass + 1))
fi
COMPACT_FAILED_COST_DIR="$(dirname "$COMPACT_FAILED_COST_LOG")"
"$SFH" run compact-failed-cost.yaml --resume "$COMPACT_FAILED_COST_DIR" -q \
  > compact-failed-cost-resume.out 2> compact-failed-cost-resume.err
check "failed compactor cost survives resume" 1 $?
if [ -e compact-failed-cost-next.marker ]; then
  echo "FAIL - resume forgot failed compactor usage and ran the next step"
  fail=$((fail + 1))
else
  echo "ok   - resumed cost guard still includes the failed compactor"
  pass=$((pass + 1))
fi

# max_total_steps covers every engine-scheduled leaf run, including recovery
# and summarization paths. Those two paths used to increment the count without
# checking it, so a limit of one still executed a second external command.
cat > max-total-fallback.yaml <<'YAML'
name: max-total-fallback
defaults:
  max_total_steps: 1
profiles:
  broken: { tool: codex, bin: "false", access: read }
  works:  { tool: codex, bin: "echo", access: read }
steps:
  - id: primary
    use: broken
    fallback: [works]
    prompt: "must not reach the fallback"
YAML
"$SFH" run max-total-fallback.yaml --runs-dir max-total-fallback-runs -q \
  > max-total-fallback.out 2> max-total-fallback.err
check "max_total_steps blocks a fallback leaf beyond the limit" 1 $?
contains "the fallback limit error names max_total_steps" "max_total_steps (1)" max-total-fallback.err
MAX_TOTAL_FB_LOG="$(find max-total-fallback-runs -type f -name 'log.jsonl' -print -quit)"
if grep -qF '"event":"fallback"' "$MAX_TOTAL_FB_LOG"; then
  echo "FAIL - a fallback was spawned beyond max_total_steps"
  fail=$((fail + 1))
else
  echo "ok   - no fallback was spawned beyond max_total_steps"
  pass=$((pass + 1))
fi
contains "the paid primary is checkpointed before the fallback limit fires" \
  '"next_fallback":"works"' "$MAX_TOTAL_FB_LOG"
MAX_TOTAL_FB_DIR="$(dirname "$MAX_TOTAL_FB_LOG")"
"$SFH" run max-total-fallback.yaml --resume "$MAX_TOTAL_FB_DIR" -q \
  > max-total-fallback-resume.out 2> max-total-fallback-resume.err
check "the blocked fallback remains resumable without rerunning primary" 1 $?
MAX_TOTAL_PRIMARY_STARTS="$(
  grep -F '"event":"step_start"' "$MAX_TOTAL_FB_LOG" | grep -cF '"step":"primary"'
)"
check "the max_total resume does not rebill the checkpointed primary" \
  1 "$MAX_TOTAL_PRIMARY_STARTS"

cat > max-total-compact.yaml <<'YAML'
name: max-total-compact
defaults:
  max_total_steps: 1
steps:
  - id: source
    cmd: ["printf", "LONG-OUTPUT-FOR-COMPACTION"]
    compact:
      when_over: 1
      tool: codex
      bin: "echo"
YAML
"$SFH" run max-total-compact.yaml --runs-dir max-total-compact-runs -q \
  > max-total-compact.out 2> max-total-compact.err
check "max_total_steps blocks a compactor leaf beyond the limit" 1 $?
contains "the compactor limit error names max_total_steps" "max_total_steps (1)" max-total-compact.err
MAX_TOTAL_COMPACT_LOG="$(find max-total-compact-runs -type f -name 'log.jsonl' -print -quit)"
if grep -qF '"event":"compact_start"' "$MAX_TOTAL_COMPACT_LOG"; then
  echo "FAIL - a compactor was spawned beyond max_total_steps"
  fail=$((fail + 1))
else
  echo "ok   - no compactor was spawned beyond max_total_steps"
  pass=$((pass + 1))
fi

# --- empty output from a cmd step is allowed, shell injection is not ----------
# With unsafe_shell_template the step opts back into shell templating; sfh then
# applies the metacharacter check, which still catches shell DELIMITERS. (That
# check is not a security boundary - see the S3-1 tests below - but under the
# opt-in it remains the agreed behaviour.)
cat > guard.yaml <<'YAML'
name: guard
vars:
  evil: "hello & echo pwned"
steps:
  - id: t
    unsafe_shell_template: true
    cmd: "echo {{vars.evil}}"
YAML
"$SFH" run guard.yaml -q > guard.out 2> guard.err
# Exit 2, not 1. The hardening moved this rejection into step preparation, so
# nothing is executed and the run never becomes a "flow that ran and failed".
# 2 is what every other rejection in this file expects - traversal flow names,
# string-cmd templating, unclosed raw blocks, an AI step without access - and
# this line was the one holdout still asserting the pre-hardening code.
check "shell metacharacters in substitutions are rejected" 2 $?
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

# --- failed output provenance banners ----------------------------------------
cat > failed-output.yaml <<'YAML'
name: failed-output
steps:
  - id: worker
    cmd: ["sh", "-c", "printf PARTIAL-LEAF; exit 9"]
    on_error: continue
  - id: consumer
    prompt: |
      CONSUMER-BEGIN
      {{steps.worker.output}}
      CONSUMER-END
    cmd: ["cat", "{{prompt_file}}"]
YAML
"$SFH" run failed-output.yaml --runs-dir failed-output-runs -q > failed-output.out 2>&1
check "a flow may continue after a failed leaf" 0 $?
FAILED_OUTPUT_DIR="$(dirname "$(find failed-output-runs -type f -name 'log.jsonl' -print -quit)")"
contains "failed leaf text is marked in the downstream prompt" "step 'worker' did not complete (exit=9, timed_out=false)" "$FAILED_OUTPUT_DIR/consumer.prompt.txt"
contains "the banner denies result status" "It is not a result." "$FAILED_OUTPUT_DIR/consumer.prompt.txt"
contains "the raw failed leaf chain is preserved" "PARTIAL-LEAF" "$FAILED_OUTPUT_DIR/worker.chain.txt"
if grep -qF -e "[sfh:" "$FAILED_OUTPUT_DIR/worker.chain.txt"; then
  echo "FAIL - a plain leaf's raw chain file contains an sfh banner"
  fail=$((fail + 1))
else
  echo "ok   - a plain leaf's raw chain file remains banner-free"
  pass=$((pass + 1))
fi

# A successful flow must not present its final failed leaf's partial text as a
# successful stdout result merely because on_error routed to end.
cat > failed-final.yaml <<'YAML'
name: failed-final
steps:
  - id: final
    cmd: ["sh", "-c", "printf MUST-NOT-EMIT; exit 9"]
    on_error: goto:end
YAML
"$SFH" run failed-final.yaml -q > failed-final.out 2> failed-final.err
check "on_error may deliberately end a flow after a failed final step" 0 $?
if [ -s failed-final.out ]; then
  echo "FAIL - failed final step was emitted as a successful result"
  sed -n '1,20p' failed-final.out
  fail=$((fail + 1))
else
  echo "ok   - failed final step is not emitted on the success path"
  pass=$((pass + 1))
fi

# --- one failed member in an eight-way fan-out -------------------------------
cat > fan-banner.yaml <<'YAML'
name: fan-banner
steps:
  - id: fan
    parallel:
      - { id: c0, cmd: ["echo", "OK-0"] }
      - { id: c1, cmd: ["echo", "OK-1"] }
      - { id: c2, cmd: ["echo", "OK-2"] }
      - id: c3
        cmd: ["sh", "-c", "printf BROKEN-3; exit 9"]
        on_error: continue
      - { id: c4, cmd: ["echo", "OK-4"] }
      - { id: c5, cmd: ["echo", "OK-5"] }
      - { id: c6, cmd: ["echo", "OK-6"] }
      - { id: c7, cmd: ["echo", "OK-7"] }
  - id: consumer
    prompt: "{{steps.fan.output}}"
    cmd: ["cat", "{{prompt_file}}"]
YAML
"$SFH" run fan-banner.yaml --runs-dir fan-banner-runs -q > fan-banner.out 2>&1
check "an eight-way fan-out may continue with one opted-out failure" 0 $?
FAN_BANNER_DIR="$(dirname "$(find fan-banner-runs -type f -name 'log.jsonl' -print -quit)")"
contains "the failed member's aggregate header is marked" "--- c3 [sfh: FAILED exit=9, timed_out=false] ---" "$FAN_BANNER_DIR/fan.chain.txt"
contains "the failed member's text has a provenance banner" "step 'c3' did not complete (exit=9, timed_out=false)" "$FAN_BANNER_DIR/fan.chain.txt"
if [ "$(grep -cF -e "did not complete" "$FAN_BANNER_DIR/fan.chain.txt")" = "1" ] &&
   [ "$(grep -cF -e "[sfh: FAILED" "$FAN_BANNER_DIR/fan.chain.txt")" = "1" ]; then
  echo "ok   - exactly one of eight fan-out members is marked"
  pass=$((pass + 1))
else
  echo "FAIL - fan-out marked the wrong number of members"
  sed -n '1,60p' "$FAN_BANNER_DIR/fan.chain.txt"
  fail=$((fail + 1))
fi
contains "a successful adjacent member stays unmarked" "--- c2 ---" "$FAN_BANNER_DIR/fan.chain.txt"

# --- live and resumed prompts are byte-identical -----------------------------
# First run live and retain the exact downstream prompt. Then trim the event log
# immediately after a successful checkpoint's step_end, simulating power loss
# before its position event. Resume must re-evaluate routing, not rerun the
# checkpoint, and must reconstruct the earlier failure banner byte-for-byte.
cat > prompt-parity.yaml <<'YAML'
name: prompt-parity
steps:
  - id: failed_source
    cmd: ["sh", "-c", "printf PARITY-PARTIAL; exit 9"]
    on_error: continue
  - id: checkpoint
    cmd: ["echo", "CHECKPOINT"]
  - id: consumer
    prompt: |
      BYTE-BEGIN
      {{steps.failed_source.output}}
      BYTE-END
    cmd: ["cat", "{{prompt_file}}"]
YAML
"$SFH" run prompt-parity.yaml --runs-dir prompt-parity-runs -q > prompt-parity-live.out 2>&1
check "the prompt parity flow runs live" 0 $?
PROMPT_PARITY_DIR="$(dirname "$(find prompt-parity-runs -type f -name 'log.jsonl' -print -quit)")"
cp "$PROMPT_PARITY_DIR/consumer.prompt.txt" prompt.live
awk '{ print } /"event":"step_end"/ && /"step":"checkpoint"/ { exit }' \
  "$PROMPT_PARITY_DIR/log.jsonl" > "$PROMPT_PARITY_DIR/log.trimmed"
mv "$PROMPT_PARITY_DIR/log.trimmed" "$PROMPT_PARITY_DIR/log.jsonl"
"$SFH" run prompt-parity.yaml --resume "$PROMPT_PARITY_DIR" -q > prompt-parity-resume.out 2>&1
check "resume continues from an unrecorded routing decision" 0 $?
if cmp -s prompt.live "$PROMPT_PARITY_DIR/consumer.prompt.txt"; then
  echo "ok   - live and resumed downstream prompts are byte-identical"
  pass=$((pass + 1))
else
  echo "FAIL - live and resumed downstream prompts differ"
  diff -u prompt.live "$PROMPT_PARITY_DIR/consumer.prompt.txt" | sed -n '1,80p'
  fail=$((fail + 1))
fi
if [ "$(grep -F '"event":"step_end"' "$PROMPT_PARITY_DIR/log.jsonl" | grep -cF '"step":"checkpoint"')" = "1" ]; then
  echo "ok   - resume re-evaluated routing without rerunning the checkpoint"
  pass=$((pass + 1))
else
  echo "FAIL - resume reran the successful checkpoint"
  fail=$((fail + 1))
fi
contains "the reconstructed routing decision is logged" '"after":"checkpoint"' "$PROMPT_PARITY_DIR/log.jsonl"

# A step_start without its step_end is different: sfh cannot know whether the
# command had side effects, so it warns and records that the command will rerun.
cat > unfinished.yaml <<'YAML'
name: unfinished
steps:
  - id: open_pr
    cmd: ["echo", "RERUN-COMMAND"]
YAML
"$SFH" run unfinished.yaml --runs-dir unfinished-runs -q > unfinished-live.out 2>&1
check "the unfinished-step fixture runs once" 0 $?
UNFINISHED_DIR="$(dirname "$(find unfinished-runs -type f -name 'log.jsonl' -print -quit)")"
awk '{ print } /"event":"step_start"/ && /"step":"open_pr"/ { exit }' \
  "$UNFINISHED_DIR/log.jsonl" > "$UNFINISHED_DIR/log.trimmed"
mv "$UNFINISHED_DIR/log.trimmed" "$UNFINISHED_DIR/log.jsonl"
"$SFH" run unfinished.yaml --resume "$UNFINISHED_DIR" -q > unfinished-resume.out 2> unfinished-resume.err
check "an unfinished first step can be resumed" 0 $?
contains "resume warns that the unfinished command will rerun" "resuming will run it again:" unfinished-resume.err
contains "status records the unfinished step id" '"step": "open_pr"' "$UNFINISHED_DIR/status.json"
contains "status records the rerun risk" '"will_rerun": true' "$UNFINISHED_DIR/status.json"

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
not_contains "timeout closes every inherited output pipe" "output drain timed out" to.out
if [ "$elapsed" -lt 25 ]; then
  echo "ok   - timeout enforced (${elapsed}s)"
  pass=$((pass + 1))
else
  echo "FAIL - timeout not enforced (${elapsed}s)"
  fail=$((fail + 1))
fi

# A per-leaf cancellation boundary must not become a per-run one: timing out
# one parallel member kills that member's descendants, not a healthy sibling.
cat > timeout-sibling.yaml <<'YAML'
name: timeout-sibling
steps:
  - id: fan
    on_error: continue
    parallel:
      - id: times_out
        cmd: ["sh", "-c", "sleep 30 & echo started; sleep 30"]
        timeout_sec: 1
        on_error: continue
      - id: survives
        cmd: ["sh", "-c", "sleep 3; echo sibling-survived"]
  - id: after
    cmd: ["echo", "fanout-continued"]
YAML
"$SFH" run timeout-sibling.yaml --runs-dir timeout-sibling-runs -q > timeout-sibling.out 2>&1
check "one member timeout does not fail the whole continued fan-out" 0 $?
SIBLING_DIR="$(dirname "$(find timeout-sibling-runs -type f -name 'log.jsonl' -print -quit)")"
contains "one member timeout does not kill a parallel sibling" "sibling-survived" "$SIBLING_DIR/survives.out.txt"
contains "flow continues after the isolated member timeout" "fanout-continued" timeout-sibling.out

# A command's background descendants belong to that leaf. Even when the root
# shell exits 0, inherited pipe handles must be reaped instead of stalling the
# drain or leaking past a normally completed run.
cat > background-exit.yaml <<'YAML'
name: background-exit
steps:
  - id: starts_background
    cmd: ["sh", "-c", "sleep 30 & echo root-finished"]
    timeout_sec: 10
  - id: after
    cmd: ["echo", "background-reaped"]
YAML
start=$(date +%s)
"$SFH" run background-exit.yaml -q > background-exit.out 2>&1
check "a successful root does not leave its background tree alive" 0 $?
contains "flow continues after reaping a successful leaf tree" "background-reaped" background-exit.out
not_contains "successful leaf cleanup closes inherited output pipes" "output drain timed out" background-exit.out
elapsed=$(( $(date +%s) - start ))
if [ "$elapsed" -lt 8 ]; then
  echo "ok   - successful leaf tree was reaped promptly (${elapsed}s)"
  pass=$((pass + 1))
else
  echo "FAIL - successful leaf tree cleanup stalled (${elapsed}s)"
  fail=$((fail + 1))
fi

# Ctrl+C is a run-level cancellation, not an ordinary leaf failure. In
# particular, on_error: goto:end must not convert an interrupted child into a
# successful run before the loop-top interrupt check runs again.
case "$(uname 2>/dev/null)" in
  MINGW*|MSYS*|CYGWIN*)
    echo "ok   - graceful SIGINT precedence is exercised on Unix CI"
    pass=$((pass + 1))
    ;;
  *)
    cat > interrupt-on-error.yaml <<'YAML'
api_version: 1
name: interrupt-on-error
steps:
  - id: slow
    cmd: ["sh", "-c", "echo ready; sleep 30"]
    on_error: goto:end
YAML
    "$SFH" run interrupt-on-error.yaml --runs-dir interrupt-on-error-runs -q \
      > interrupt-on-error.out 2> interrupt-on-error.err &
    INTERRUPT_PID=$!
    INTERRUPT_LOG=""
    for _ in $(seq 1 100); do
      INTERRUPT_LOG="$(find interrupt-on-error-runs -type f -name 'log.jsonl' -print -quit 2>/dev/null)"
      if [ -n "$INTERRUPT_LOG" ] &&
          grep -qF '"event":"step_start"' "$INTERRUPT_LOG" 2>/dev/null; then
        break
      fi
      sleep 0.05
    done
    kill -INT "$INTERRUPT_PID" 2>/dev/null
    wait "$INTERRUPT_PID"
    INTERRUPT_RC=$?
    check "Ctrl+C cannot be converted to success by on_error goto:end" 1 "$INTERRUPT_RC"
    contains "the interrupted leaf remains diagnostic, not completed" \
      '"interrupted":true' "$INTERRUPT_LOG"
    contains "the cancelled run records a failed terminal state" \
      '"status":"failed"' "$INTERRUPT_LOG"
    ;;
esac

# --- detach / status / wait ---------------------------------------------------
# The whole point of --detach is that the run outlives whoever started it, so
# these checks care about two things: the launcher returns at once, and the run
# is still there afterwards.
cat > detach.yaml <<'YAML'
name: detach
steps:
  - id: think
    cmd: ["sh", "-c", "sleep 6; echo SLOW-STEP-DONE"]
  - id: answer
    cmd: ["echo", "DETACHED:{{steps.think.output | trim}}"]
YAML
start=$(date +%s)
# Captured through $(...) on purpose: a child holding this pipe open would make
# the substitution block for the whole run, which is the bug this guards.
RUN_DIR="$("$SFH" run detach.yaml --detach -q 2>d.err)"
launch_secs=$(( $(date +%s) - start ))
if [ "$launch_secs" -le 2 ] && [ -n "$RUN_DIR" ]; then
  echo "ok   - --detach returns immediately (${launch_secs}s)"
  pass=$((pass + 1))
else
  echo "FAIL - --detach blocked for ${launch_secs}s (run dir: '$RUN_DIR')"
  sed -n '1,20p' d.err
  fail=$((fail + 1))
fi

"$SFH" status "$RUN_DIR" > st1.out 2>&1
check "status reports a live run as running" 3 $?
contains "status names the current step" "running" st1.out

"$SFH" status "$RUN_DIR" --json > st1.json 2>/dev/null
contains "status --json is machine readable" '"state": "running"' st1.json

"$SFH" wait "$RUN_DIR" > w.out 2>w.err
check "wait blocks until the run finishes" 0 $?
contains "wait prints the flow result" "DETACHED:SLOW-STEP-DONE" w.out
not_contains "successful wait does not append a diagnostic footer" "sfh: done." w.err

"$SFH" status "$RUN_DIR" > st2.out 2>&1
check "status reports a finished run as done" 0 $?

# A run whose process is gone must read as dead, not as still running: this is
# what tells a caller its work was killed rather than merely slow.
RUN_DIR2="$("$SFH" run detach.yaml --detach -q 2>/dev/null)"
DPID="$(sed -n 's/.*"pid": *\([0-9]*\).*/\1/p' "$RUN_DIR2/status.json")"
if [ -n "$DPID" ]; then
  kill -9 "$DPID" 2>/dev/null || taskkill //PID "$DPID" //T //F >/dev/null 2>&1
  sleep 1
  "$SFH" status "$RUN_DIR2" > st3.out 2>&1
  check "a killed run reports dead, not running" 1 $?
  contains "dead status explains how to resume" "--resume" st3.out
  "$SFH" wait "$RUN_DIR2" > w2.out 2>&1
  check "wait on a dead run returns instead of hanging" 1 $?
else
  echo "FAIL - could not read the detached pid from status.json"
  fail=$((fail + 1))
fi

# --- fallback: works inside a fan-out, not just on a plain step ---------------
# It used to be honoured only on plain leaves: a parallel child could declare
# fallback and silently not have it. Fake tools, so no AI is called - `false`
# always fails, `echo` always succeeds and codex's parser falls back to stdout.
cat > fb.yaml <<'YAML'
name: fb
profiles:
  broken: { tool: codex, bin: "false", access: read }
  works:  { tool: codex, bin: "echo", access: read }
steps:
  - id: plain
    use: broken
    fallback: [works]
    prompt: "plain"
  - id: fan
    parallel:
      - id: kid
        use: broken
        fallback: [works]
        prompt: "hi"
  - id: after
    cmd: ["echo", "fanout-survived"]
YAML
"$SFH" run fb.yaml --runs-dir fb-runs > fb.out 2>fb.err
check "plain and parallel leaves fall back instead of failing the flow" 0 $?
contains "the flow continued past the fan-out" "fanout-survived" fb.out
contains "the fallback profile actually ran" "falling back to profile 'works'" fb.err
FB_LOG="$(find fb-runs -type f -name 'log.jsonl' -print -quit)"
FB_STEP_ENDS="$(grep -cF '"event":"step_end"' "$FB_LOG")"
contains "plain fallback logs its failed attempt" '"exit":1' "$FB_LOG"
if [ "$FB_STEP_ENDS" = "5" ]; then
  echo "ok   - every fallback attempt has a step_end"
  pass=$((pass + 1))
else
  echo "FAIL - expected 5 step_end events, found $FB_STEP_ENDS"
  fail=$((fail + 1))
fi
contains "meta leaf_runs agrees with step_end count" "\"leaf_runs\": $FB_STEP_ENDS" "$(dirname "$FB_LOG")/meta.json"

# A crash after the primary failure was made durable but before its selected
# fallback finished used to replay on_error and skip the fallback altogether.
# Rewind a completed run to exactly that checkpoint: resume must execute the
# selected profile in the SAME visit and must not run/pay the broken primary a
# second time.
cp -r "$(dirname "$FB_LOG")" fb-checkpoint
awk '{ print } /"event":"step_end"/ && /"step":"plain"/ { exit }' \
  "$FB_LOG" > fb-checkpoint/log.jsonl
"$SFH" run fb.yaml --resume fb-checkpoint -q > fb-resume.out 2> fb-resume.err
check "a crash at a fallback checkpoint resumes successfully" 0 $?
contains "resume continues the selected fallback directly" \
  "resuming that fallback directly" fb-resume.err
contains "the resumed fallback remains in the original visit" \
  '"profile":"works","resumed":true' fb-checkpoint/log.jsonl
FB_PRIMARY_FAILURES="$(grep -F '"event":"step_end"' fb-checkpoint/log.jsonl |
  grep -F '"step":"plain"' | grep -cF '"exit":1')"
check "resume does not rerun the already billed broken primary" 1 "$FB_PRIMARY_FAILURES"
contains "the flow continues after the resumed fallback" "fanout-survived" fb-resume.out

# The same crash boundary exists inside fan-out members. Their primary
# step_end must identify the selected fallback so resuming the GROUP continues
# that member's paid chain instead of starting its primary profile over.
cp -r "$(dirname "$FB_LOG")" fb-fan-checkpoint
awk '{ print } /"event":"step_end"/ && /"step":"kid"/ && /"next_fallback":"works"/ { exit }' \
  "$FB_LOG" > fb-fan-checkpoint/log.jsonl
"$SFH" run fb.yaml --resume fb-fan-checkpoint -q \
  > fb-fan-resume.out 2> fb-fan-resume.err
check "a parallel member resumes from its fallback checkpoint" 0 $?
contains "parallel fallback resume is explicit in the log" \
  '"parent":"fan","profile":"works","resumed":true' fb-fan-checkpoint/log.jsonl
FB_KID_PRIMARY_FAILURES="$(
  grep -F '"event":"step_end"' fb-fan-checkpoint/log.jsonl |
    grep -F '"step":"kid"' | grep -cF '"exit":1'
)"
check "parallel fallback resume does not rebill the member primary" \
  1 "$FB_KID_PRIMARY_FAILURES"
contains "parallel fallback resume continues beyond the group" \
  "fanout-survived" fb-fan-resume.out

cat > fb-foreach.yaml <<'YAML'
api_version: 1
name: fb-foreach
profiles:
  broken: {tool: codex, bin: "false", access: read}
  works: {tool: codex, bin: "echo", access: read}
steps:
  - id: source
    cmd: ["echo", "one-item"]
  - id: fan
    use: broken
    fallback: [works]
    foreach: {from: "{{steps.source.output}}", split: lines}
    prompt: "{{item}}"
  - id: after
    cmd: ["echo", "foreach-survived"]
YAML
"$SFH" run fb-foreach.yaml --runs-dir fb-foreach-runs -q \
  > fb-foreach.out 2> fb-foreach.err
check "the foreach fallback fixture completes normally" 0 $?
FB_FOREACH_LOG="$(
  find fb-foreach-runs -type f -name 'log.jsonl' -print -quit
)"
cp -r "$(dirname "$FB_FOREACH_LOG")" fb-foreach-checkpoint
awk '{ print } index($0, "\"event\":\"step_end\"") && index($0, "\"step\":\"fan[0]\"") && index($0, "\"next_fallback\":\"works\"") { exit }' \
  "$FB_FOREACH_LOG" > fb-foreach-checkpoint/log.jsonl
"$SFH" run fb-foreach.yaml --resume fb-foreach-checkpoint -q \
  > fb-foreach-resume.out 2> fb-foreach-resume.err
check "a foreach member resumes from its fallback checkpoint" 0 $?
contains "foreach fallback resume is explicit in the log" \
  '"parent":"fan","profile":"works","resumed":true' fb-foreach-checkpoint/log.jsonl
FB_ITEM_PRIMARY_FAILURES="$(
  grep -F '"event":"step_end"' fb-foreach-checkpoint/log.jsonl |
    grep -F '"step":"fan[0]"' | grep -cF '"exit":1'
)"
check "foreach fallback resume does not rebill the item primary" \
  1 "$FB_ITEM_PRIMARY_FAILURES"
contains "foreach fallback resume continues beyond the group" \
  "foreach-survived" fb-foreach-resume.out

# --- {{raw}} lets a prompt talk about templates -------------------------------
# Checked through validate/--dry-run rather than a command's output: msys2's
# echo brace-expands {{x}} on its own, which would test the shell, not sfh.
cat > raw.yaml <<'YAML'
name: raw
steps:
  - id: a
    cmd: ["echo", "{{raw}}{{user.name}} {{#each xs}}{{endraw}}"]
YAML
"$SFH" validate raw.yaml > raw.out 2>&1
check "a literal {{ does not fail the precheck" 0 $?
"$SFH" run raw.yaml --dry-run > rawdry.out 2>&1
contains "raw block passed the braces through" "{{user.name}} {{#each xs}}" rawdry.out
# ...and an unclosed raw block is still an error, with the fix named.
cat > rawbad.yaml <<'YAML'
name: rawbad
steps:
  - id: a
    cmd: ["echo", "{{raw}}never closed"]
YAML
"$SFH" validate rawbad.yaml > rawbad.out 2>&1
check "an unclosed raw block is rejected" 2 $?
contains "and says how to close it" "endraw" rawbad.out

# --- sfh stop kills the whole tree, not just sfh ------------------------------
# The guarantee is "nothing outlives sfh". It has been broken once already, by
# granting the job object BREAKAWAY_OK - which msys2's sh uses, so shell steps
# quietly escaped. A surviving grandchild is a process still burning money, so
# this checks the grandchild, not sfh.
# A GRANDCHILD ticks a counter file, so "did it really die" is answered by
# whether work is still happening - no pid plumbing, same check on every OS.
# It has to be a grandchild: the direct child is assigned to the job explicitly
# and cannot escape, so testing it would have missed the real bug entirely.
cat > longrun.yaml <<'YAML'
name: longrun
steps:
  - id: think
    cmd: ["sh", "-c", "sh -c 'i=0; while [ $i -lt 600 ]; do i=$((i+1)); echo $i > ticks.txt; sleep 1; done' & sleep 600"]
YAML
rm -f ticks.txt
RUN_DIR3="$("$SFH" run longrun.yaml --detach -q 2>/dev/null)"
sleep 3
"$SFH" stop "$RUN_DIR3" > stop.out 2> stop.err
check "stop reports success" 0 $?
contains "stop says what it killed" "killed pid" stop.out
not_contains "successful stop does not split its report across streams" "killed pid" stop.err
TICK_AT_STOP="$(cat ticks.txt 2>/dev/null || echo none)"
sleep 3
TICK_LATER="$(cat ticks.txt 2>/dev/null || echo none)"
"$SFH" status "$RUN_DIR3" > st4.out 2>&1
check "a stopped run reports stopped" 1 $?
contains "stopped status is not confused with a crash" "stopped" st4.out
if [ "$TICK_AT_STOP" = "none" ]; then
  echo "FAIL - the grandchild never started, so this proves nothing"
  fail=$((fail + 1))
elif [ "$TICK_AT_STOP" = "$TICK_LATER" ]; then
  echo "ok   - stop killed the grandchild too (ticks frozen at $TICK_AT_STOP)"
  pass=$((pass + 1))
else
  echo "FAIL - grandchild outlived sfh stop: ticks went $TICK_AT_STOP -> $TICK_LATER"
  fail=$((fail + 1))
fi

# --- normalized exit/stderr templates survive resume -------------------------
cat > diagnostics.yaml <<'YAML'
name: diagnostics
steps:
  - id: check
    cmd: ["sh", "-c", "echo DETERMINISTIC-DIAGNOSTIC >&2; exit 7"]
    on_error: goto:fix
  - id: fix
    cmd: ["sh", "-c", "exit 9"]
YAML
"$SFH" run diagnostics.yaml --runs-dir diagnostics-runs -q > diagnostics1.out 2>&1
check "the initial repair flow stops at its deliberately broken fixer" 1 $?
DIAGNOSTICS_LOG="$(find diagnostics-runs -type f -name 'log.jsonl' -print -quit)"
DIAGNOSTICS_DIR="$(dirname "$DIAGNOSTICS_LOG")"
contains "on_error routing records its reason" '"via":"on_error"' "$DIAGNOSTICS_LOG"
cat > diagnostics.yaml <<'YAML'
name: diagnostics
steps:
  - id: check
    cmd: ["sh", "-c", "echo DETERMINISTIC-DIAGNOSTIC >&2; exit 7"]
    on_error: goto:fix
  - id: fix
    cmd: ["echo", "exit={{steps.check.exit}} err={{steps.check.stderr_file}}"]
  - id: read_stderr
    cmd: ["cat", "{{steps.check.stderr_file}}"]
YAML
"$SFH" run diagnostics.yaml --runs-dir diagnostics-runs --resume-latest --force-resume -q > diagnostics2.out 2>&1
check "resume restores diagnostic template fields" 0 $?
FIX_OUTPUT="$(find "$DIAGNOSTICS_DIR" -type f -name 'fix.v*.out.txt' -print -quit)"
contains "resume restores sfh's normalized exit" "exit=7" "$FIX_OUTPUT"
contains "resume restores the stderr file path" "check.err.txt" "$FIX_OUTPUT"
contains "the restored stderr file contains the diagnostic" "DETERMINISTIC-DIAGNOSTIC" diagnostics2.out

# --- retries preserve every attempt's artifacts ------------------------------
cat > retry-once.sh <<'SH'
#!/usr/bin/env sh
if [ ! -f retry-marker ]; then
  echo FIRST-STDOUT
  echo FIRST-STDERR >&2
  touch retry-marker
  exit 1
fi
echo SECOND-STDOUT
echo SECOND-STDERR >&2
SH
cat > retry.yaml <<'YAML'
name: retry
steps:
  - id: retry
    cmd: ["sh", "retry-once.sh"]
    retry: { max: 1, backoff_sec: 0 }
    retry_on: any
YAML
"$SFH" run retry.yaml --runs-dir retry-runs -q > retry.out 2>&1
check "a retry can recover the step" 0 $?
RETRY_DIR="$(dirname "$(find retry-runs -type f -name 'log.jsonl' -print -quit)")"
contains "the first attempt stdout is preserved" "FIRST-STDOUT" "$RETRY_DIR/retry.out.txt"
contains "the second attempt stdout has an a2 artifact" "SECOND-STDOUT" "$RETRY_DIR/retry.a2.out.txt"
contains "the first attempt stderr is preserved" "FIRST-STDERR" "$RETRY_DIR/retry.err.txt"
contains "the second attempt stderr has an a2 artifact" "SECOND-STDERR" "$RETRY_DIR/retry.a2.err.txt"
contains "the second attempt chain has an a2 artifact" "SECOND-STDOUT" "$RETRY_DIR/retry.a2.chain.txt"

# Every attempt is billable even though retries are represented by one
# step_end. Before v1.1.1 the successful attempt replaced the failed one's
# Usage, so two $0.10 attempts looked like $0.10 and the next step ran under a
# $0.15 ceiling.
rm -f retry-cost.marker retry-cost-next.marker
cat > retry-cost.yaml <<YAML
name: retry-cost
defaults:
  max_cost_usd: 0.15
steps:
  - id: paid
    tool: claude
    bin: "$STUB_BIN"
    access: read
    retry: { max: 1, backoff_sec: 0 }
    retry_on: any
    env:
      SFH_STUB_COST: "0.10"
      SFH_STUB_FAIL_ONCE: "retry-cost.marker"
    prompt: "fail once, then recover"
  - id: forbidden
    cmd: ["sh", "-c", "printf 'ran\n' > retry-cost-next.marker"]
YAML
"$SFH" run retry-cost.yaml --runs-dir retry-cost-runs -q > retry-cost.out 2> retry-cost.err
check "retry accounting stops before the next step after two paid attempts" 1 $?
RETRY_COST_LOG="$(find retry-cost-runs -type f -name 'log.jsonl' -print -quit)"
contains "retry accounting records both attempts" '"attempts":2' "$RETRY_COST_LOG"
contains "retry accounting accumulates both costs" '"cost_usd":0.2' "$RETRY_COST_LOG"
contains "retry accounting accumulates input tokens" '"input_tokens":22' "$RETRY_COST_LOG"
contains "retry accounting accumulates output tokens" '"output_tokens":14' "$RETRY_COST_LOG"
if [ -e retry-cost-next.marker ]; then
  echo "FAIL - the cost guard let the post-retry step run"
  fail=$((fail + 1))
else
  echo "ok   - the cost guard blocked the post-retry step"
  pass=$((pass + 1))
fi

# A provider can finish and bill successfully while publication of one required
# result artifact fails (bad filesystem entry, disk error, AV/share failure).
# The charge must survive in run accounting, and the incomplete checkpoint must
# make this run non-resumable instead of executing the uncertain side effect a
# second time.
cat > persistence-failure-cost.yaml <<YAML
api_version: 1
name: persistence-failure-cost
steps:
  - id: paid
    tool: claude
    bin: "$STUB_BIN"
    access: read
    env:
      SFH_STUB_COST: "0.25"
      SFH_STUB_MKDIR: "{{run_dir}}/paid.chain.txt"
    prompt: "finish, then make the chain target unpublishable"
YAML
"$SFH" run persistence-failure-cost.yaml --runs-dir persistence-failure-cost-runs -q \
  > persistence-failure-cost.out 2> persistence-failure-cost.err
check "a required artifact publication failure fails the run" 1 $?
PERSISTENCE_FAILURE_LOG="$(
  find persistence-failure-cost-runs -type f -name 'log.jsonl' -print -quit
)"
PERSISTENCE_FAILURE_DIR="$(dirname "$PERSISTENCE_FAILURE_LOG")"
contains "the paid but unpublishable attempt gets a durable failure marker" \
  '"event":"persistence_failure"' "$PERSISTENCE_FAILURE_LOG"
contains "the persistence marker records the provider charge" \
  '"cost_usd":0.25' "$PERSISTENCE_FAILURE_LOG"
contains "final metadata does not erase cost after artifact failure" \
  '"cost_usd": 0.25' "$PERSISTENCE_FAILURE_DIR/meta.json"
"$SFH" run persistence-failure-cost.yaml --resume "$PERSISTENCE_FAILURE_DIR" -q \
  > persistence-failure-resume.out 2> persistence-failure-resume.err
check "an uncertain paid persistence failure is not automatically re-executed" 2 $?
contains "resume explains why repeating the paid side effect is unsafe" \
  "run is non-resumable" persistence-failure-resume.err
PERSISTENCE_FAILURE_STARTS="$(
  grep -cF '"event":"step_start"' "$PERSISTENCE_FAILURE_LOG"
)"
check "refused resume did not start the paid step twice" 1 "$PERSISTENCE_FAILURE_STARTS"

# A provider's negative report is invalid accounting input, not a refund. The
# old live path added it verbatim even though resume clamped the same record.
rm -f negative-cost-next.marker
cat > negative-cost.yaml <<YAML
name: negative-cost
defaults:
  max_cost_usd: 0.10
steps:
  - id: positive_one
    tool: claude
    bin: "$STUB_BIN"
    access: read
    env: { SFH_STUB_COST: "0.09" }
    prompt: "first charge"
  - id: invalid_refund
    tool: claude
    bin: "$STUB_BIN"
    access: read
    env: { SFH_STUB_COST: "-0.09" }
    prompt: "invalid refund"
  - id: positive_two
    tool: claude
    bin: "$STUB_BIN"
    access: read
    env: { SFH_STUB_COST: "0.09" }
    prompt: "second charge"
  - id: forbidden
    cmd: ["sh", "-c", "printf 'ran\n' > negative-cost-next.marker"]
YAML
"$SFH" run negative-cost.yaml --runs-dir negative-cost-runs -q > negative-cost.out 2> negative-cost.err
check "a negative provider cost cannot refund earlier spend" 1 $?
NEGATIVE_COST_LOG="$(find negative-cost-runs -type f -name 'log.jsonl' -print -quit)"
contains "the invalid cost is normalized in the durable log" '"cost_usd":0.0' "$NEGATIVE_COST_LOG"
contains "the invalid report is visible to the operator" "invalid cost_usd" negative-cost.err
if [ -e negative-cost-next.marker ]; then
  echo "FAIL - a negative report refunded spend and let the next step run"
  fail=$((fail + 1))
else
  echo "ok   - negative reported cost did not refund spend"
  pass=$((pass + 1))
fi

# Runtime validation must match the JSON schema. Non-finite/negative ceilings
# used to parse successfully; NaN then made every `cost > limit` check false.
for bad_cost in -0.01 .nan .inf; do
  cat > "bad-cost-${bad_cost//[^A-Za-z0-9]/_}.yaml" <<YAML
name: bad-cost
defaults:
  max_cost_usd: $bad_cost
steps:
  - id: one
    cmd: ["echo", "one"]
YAML
  BAD_COST_FILE="bad-cost-${bad_cost//[^A-Za-z0-9]/_}.yaml"
  "$SFH" validate "$BAD_COST_FILE" > bad-cost.out 2> bad-cost.err
  check "validate rejects max_cost_usd=$bad_cost" 2 $?
  contains "the bad ceiling error names max_cost_usd" "max_cost_usd" bad-cost.err
done

cat > zero-tool-limit.yaml <<'YAML'
name: zero-tool-limit
defaults:
  tool_max_parallel: { claude: 0 }
steps:
  - id: one
    cmd: ["echo", "one"]
YAML
"$SFH" validate zero-tool-limit.yaml > zero-tool-limit.out 2> zero-tool-limit.err
check "validate rejects a tool concurrency limit that would deadlock" 2 $?
contains "the zero-limit error explains the permanent block" "block that tool forever" zero-tool-limit.err

for group_field in "retry_on: any" "hang_after_sec: 0"; do
  cat > bad-group-leaf-setting.yaml <<YAML
name: bad-group-leaf-setting
steps:
  - id: fan
    $group_field
    parallel:
      - id: child
        cmd: ["echo", "child"]
YAML
  "$SFH" validate bad-group-leaf-setting.yaml > bad-group.out 2> bad-group.err
  check "parallel group rejects ignored leaf setting '$group_field'" 2 $?
  contains "the group-setting error lists the allowed surface" "carries only" bad-group.err
done

# --- runs subcommands ---------------------------------------------------------
"$SFH" runs list > runs.out 2>&1
check "runs list works" 0 $?
"$SFH" runs list --runs-dir visits-runs --json > runs-list.json 2>&1
check "runs list --json works" 0 $?
contains "runs list reports the maximum visit" '"visit": 2' runs-list.json
contains "runs list derives consecutive repeated outputs" '"repeat": 1' runs-list.json
contains "runs list JSON includes the selected cost footer" '"total_cost_usd": 0.0' runs-list.json
VISITS_DIR="$(dirname "$VISITS_LOG")"
"$SFH" runs show "$VISITS_DIR" --json > runs-show.json 2>&1
check "runs show --json works" 0 $?
contains "runs show reports per-step visits" '"step": "loop"' runs-show.json
contains "runs show reports per-step repeats" '"repeat": 1' runs-show.json
"$SFH" runs clean --older-than 3650 --keep 1 --dry-run > clean.out 2>&1
check "runs clean --dry-run works" 0 $?
"$SFH" runs list --limit nope > bad-limit.out 2>&1
check "runs list rejects a non-numeric long limit" 2 $?
contains "the long limit error names the flag the user typed" "--limit needs a number" bad-limit.out
"$SFH" runs clean --older-than 30dd --dry-run > bad-age.out 2>&1
check "runs clean rejects repeated day suffixes" 2 $?
contains "the age error explains the expected value" "--older-than needs days" bad-age.out
mkdir -p empty-runs
"$SFH" runs list --runs-dir empty-runs > empty-runs.out 2> empty-runs.err
check "runs list handles an empty directory" 0 $?
contains "an empty run list describes itself on stdout" "no runs under" empty-runs.out
not_contains "an empty run list never reports negative zero cost" '$-0.0000' empty-runs.out
not_contains "a successful empty run list does not split its report" "no runs under" empty-runs.err
"$SFH" runs clean --runs-dir empty-runs --dry-run > empty-clean.out 2> empty-clean.err
check "runs clean handles an empty directory" 0 $?
contains "a no-op clean report stays on stdout" "nothing to clean" empty-clean.out
not_contains "a no-op clean does not split its report" "nothing to clean" empty-clean.err

# --- a blocking human gate can use a run-local answer file -------------------
cat > human-gate.yaml <<'YAML'
name: human-gate
steps:
  - id: approve
    cmd: ["sh", "-c", "cat >&2; while [ ! -s \"$1\" ]; do sleep 1; done; cat \"$1\"", "human-gate", "{{run_dir}}/approval.txt"]
    stdin: prompt
    prompt: |
      release を承認するなら approval.txt に理由を書いてください。
    timeout_sec: 5
    on_error: goto:expired
  - id: accepted
    cmd: ["echo", "accepted={{steps.approve.output | trim}}"]
    route: [{goto: end}]
  - id: expired
    cmd: ["echo", "approval-expired"]
YAML
(while [ ! -d human-gate-run ]; do sleep 0.1; done; printf 'APPROVE-42\n' > human-gate-run/approval.txt) &
"$SFH" run human-gate.yaml --run-dir human-gate-run -q > human-gate.out 2>&1
check "a run-local human gate blocks and then continues" 0 $?
contains "the human answer becomes chain output" "accepted=APPROVE-42" human-gate.out
contains "stdin prompt records what the human must inspect" "release を承認" human-gate-run/approve.err.txt

# --- S1-1: sfh wait must not emit files outside the run dir -------------------
# Attacker crafts a status.json whose emit_file points at a secret outside the
# run dir. `sfh wait` reads the status and must refuse to print that file.
mkdir -p s11-run
echo "TOP-SECRET-S11" > s11-secret.txt
cat > s11-run/status.json <<JSON
{
  "state": "done",
  "current_step": "x",
  "started_utc": "20250101-000000",
  "heartbeat_utc": "20250101-000000",
  "steps_done": 1,
  "cost_usd": 0.0,
  "run_dir": "s11-run",
  "flow": "x.yaml",
  "pid": 99999,
  "sfh_version": "0.9.0",
  "exit_code": 0,
  "emit_step": "x",
  "emit_file": "../s11-secret.txt",
  "error": null,
  "unfinished_step": null,
  "nonce": "deadbeef"
}
JSON
# The run dir needs a matching sfh-nonce, or F-6's tamper check refuses first
# and this test never reaches the containment check it is named for. The nonce
# file is "<pid> <nonce>"; F-6 has its own tests for the mismatch cases.
printf '99999 deadbeef\n' > s11-run/sfh-nonce
"$SFH" wait s11-run > s11.out 2>s11.err
s11_rc=$?
if grep -qF "TOP-SECRET-S11" s11.out; then
  echo "FAIL - S1-1: sfh wait emitted a file outside the run dir"
  fail=$((fail + 1))
else
  echo "ok   - S1-1: sfh wait refuses to emit files outside the run dir"
  pass=$((pass + 1))
fi
if [ "$s11_rc" -ne 0 ]; then
  echo "ok   - S1-1: sfh wait exits non-zero on a containment violation"
  pass=$((pass + 1))
else
  echo "FAIL - S1-1: sfh wait exited 0 despite a containment violation"
  fail=$((fail + 1))
fi
contains "S1-1: the refusal is reported" "refused to emit" s11.err

# S1-1 symlink: a symlink at the fixed name detached.out.txt pointing outside
# the run dir must be refused the same way. Needs real symlinks: where the
# filesystem cannot create them (Windows without developer mode) an attacker
# cannot plant them either, so the check is skipped there.
if have_symlinks; then
  mkdir -p s11-sym-run
  echo "TOP-SECRET-S11-SYM" > s11-sym-secret.txt
  MSYS=winsymlinks:nativestrict ln -sf "$(pwd)/s11-sym-secret.txt" s11-sym-run/detached.out.txt
  cat > s11-sym-run/status.json <<JSON
{
  "state": "done",
  "current_step": "x",
  "started_utc": "20250101-000000",
  "heartbeat_utc": "20250101-000000",
  "steps_done": 1,
  "cost_usd": 0.0,
  "run_dir": "s11-sym-run",
  "flow": "x.yaml",
  "pid": 99999,
  "sfh_version": "0.9.0",
  "exit_code": 0,
  "emit_step": null,
  "emit_file": null,
  "error": null,
  "unfinished_step": null,
  "nonce": "deadbeef"
}
JSON
  "$SFH" wait s11-sym-run > s11-sym.out 2>s11-sym.err
  s11s_rc=$?
  if grep -qF "TOP-SECRET-S11-SYM" s11-sym.out; then
    echo "FAIL - S1-1: sfh wait followed an outward symlink in detached.out.txt"
    fail=$((fail + 1))
  else
    echo "ok   - S1-1: sfh wait refuses an outward symlink at detached.out.txt"
    pass=$((pass + 1))
  fi
  if [ "$s11s_rc" -ne 0 ]; then
    echo "ok   - S1-1: symlink violation exits non-zero"
    pass=$((pass + 1))
  else
    echo "FAIL - S1-1: symlink violation exited 0"
    fail=$((fail + 1))
  fi
else
  echo "ok   - S1-1: outward symlink at detached.out.txt (skipped: no native symlink support)"
  pass=$((pass + 1))
  echo "ok   - S1-1: symlink violation exit code (skipped: no native symlink support)"
  pass=$((pass + 1))
fi

# --- S1-1b: sfh wait must not accept an unauthenticated terminal state --------
# rev_break #7: a forged status.json that claims done/exit 0 but has no matching
# sfh-nonce file must NOT be reported as success to a caller/CI. The emit path is
# clean here (no emit_file), so the nonce authentication is what must refuse it.
mkdir -p s11b-run
cat > s11b-run/status.json <<JSON
{
  "state": "done",
  "current_step": "x",
  "started_utc": "20250101-000000",
  "heartbeat_utc": "20250101-000000",
  "steps_done": 1,
  "cost_usd": 0.0,
  "run_dir": "s11b-run",
  "flow": "x.yaml",
  "pid": 99999,
  "sfh_version": "0.9.0",
  "exit_code": 0,
  "emit_step": null,
  "emit_file": null,
  "error": null,
  "unfinished_step": null,
  "nonce": "forged-nonce"
}
JSON
"$SFH" wait s11b-run > s11b.out 2> s11b.err
check "S1-1b: a forged done status with no matching nonce is not a success" 1 $?
contains "S1-1b: the refusal names the nonce problem" "nonce" s11b.err

# --- S1-2: sfh stop must not kill unrelated processes -------------------------
# A forged status.json naming a LIVE unrelated process must not result in a
# kill. The victim is a process this test spawned, so "is it still alive?" is
# answered the same way on every OS (kill -0), unlike pid 1, which Windows
# cannot signal and would report dead no matter what sfh did.
sleep 30 &
S12_VICTIM=$!
S12_WINPID="$(winpid_of "$S12_VICTIM")"
mkdir -p s12-run
cat > s12-run/status.json <<JSON
{
  "state": "running",
  "current_step": "x",
  "started_utc": "20250101-000000",
  "heartbeat_utc": "20250101-000000",
  "steps_done": 0,
  "cost_usd": 0.0,
  "run_dir": "s12-run",
  "flow": "x.yaml",
  "pid": $S12_WINPID,
  "sfh_version": "0.9.0",
  "exit_code": null,
  "emit_step": null,
  "emit_file": null,
  "error": null,
  "unfinished_step": null,
  "nonce": "fake-nonce"
}
JSON
"$SFH" stop s12-run > s12.out 2>s12.err
if grep -qF "killed pid" s12.out || grep -qF "killed pid" s12.err; then
  echo "FAIL - S1-2: sfh stop killed a process from a fake run dir"
  fail=$((fail + 1))
else
  echo "ok   - S1-2: sfh stop does not kill from a fake run dir"
  pass=$((pass + 1))
fi
# The unrelated process named in the forged status must still be alive.
if kill -0 "$S12_VICTIM" 2>/dev/null; then
  echo "ok   - S1-2: the unrelated process survived the stop attempt"
  pass=$((pass + 1))
else
  echo "FAIL - S1-2: the unrelated process is gone (sfh killed it)"
  fail=$((fail + 1))
fi
kill "$S12_VICTIM" 2>/dev/null
wait "$S12_VICTIM" 2>/dev/null

# A real detached run with a corrupted nonce must be refused outright.
cat > s12-long.yaml <<'YAML'
name: s12-long
steps:
  - id: think
    cmd: ["sh", "-c", "sleep 60"]
YAML
S12_DIR="$("$SFH" run s12-long.yaml --detach -q 2>/dev/null)"
sleep 1
echo "corrupted" > "$S12_DIR/sfh-nonce"
"$SFH" stop "$S12_DIR" > s12b.out 2>s12b.err
rc2=$?
if [ "$rc2" -ne 0 ]; then
  echo "ok   - S1-2: sfh stop refuses a run with a corrupted nonce"
  pass=$((pass + 1))
else
  echo "FAIL - S1-2: sfh stop accepted a corrupted nonce"
  fail=$((fail + 1))
fi
contains "S1-2: corrupted nonce refusal is reported" "refusing" s12b.err
# Clean up the still-running detached process
S12_PID="$(sed -n 's/.*"pid": *\([0-9]*\).*/\1/p' "$S12_DIR/status.json")"
kill "$S12_PID" 2>/dev/null || taskkill //PID "$S12_PID" //T //F >/dev/null 2>&1

# With a MATCHING nonce on both sides and a MATCHING pid in the nonce file, the
# nonce checks all pass and the decision must fall to the executable-name
# comparison (pid_is_sfh). Without this case the strict stem match would never
# be exercised: the fixtures above are refused earlier, on the nonce itself.
sleep 30 &
S12C_VICTIM=$!
S12C_WINPID="$(winpid_of "$S12C_VICTIM")"
mkdir -p s12c-run
cat > s12c-run/status.json <<JSON
{
  "state": "running",
  "current_step": "x",
  "started_utc": "20250101-000000",
  "heartbeat_utc": "20250101-000000",
  "steps_done": 0,
  "cost_usd": 0.0,
  "run_dir": "s12c-run",
  "flow": "x.yaml",
  "pid": $S12C_WINPID,
  "sfh_version": "0.9.0",
  "exit_code": null,
  "emit_step": null,
  "emit_file": null,
  "error": null,
  "unfinished_step": null,
  "nonce": "shared-nonce-s12c"
}
JSON
echo "$S12C_WINPID shared-nonce-s12c" > s12c-run/sfh-nonce
echo '{"ts":"20250101-000000","event":"run_start"}' > s12c-run/log.jsonl
"$SFH" stop s12c-run > s12c.out 2>s12c.err
check "S1-2: a matching nonce+pid on a non-sfh process is refused" 1 $?
contains "S1-2: the refusal is the executable check, not the nonce" "not running the same sfh executable" s12c.err
if kill -0 "$S12C_VICTIM" 2>/dev/null; then
  echo "ok   - S1-2: the process that passed every nonce check survived"
  pass=$((pass + 1))
else
  echo "FAIL - S1-2: a non-sfh process with a matching nonce was killed"
  fail=$((fail + 1))
fi
kill "$S12C_VICTIM" 2>/dev/null
wait "$S12C_VICTIM" 2>/dev/null

# --- S1-3: flow name must not escape the runs root ----------------------------
cat > s13.yaml <<'YAML'
name: x/../../ESCAPED-S13
steps:
  - id: a
    cmd: ["echo", "hi"]
YAML
"$SFH" run s13.yaml --dry-run --runs-dir s13-runs > s13.out 2>s13.err
check "S1-3: a traversal flow name is rejected" 2 $?
contains "S1-3: the error names the path-separator rule" "path separators" s13.err
if [ -d "s13-runs/../ESCAPED-S13" ] || [ -d "ESCAPED-S13" ]; then
  echo "FAIL - S1-3: a directory was created outside the runs root"
  fail=$((fail + 1))
else
  echo "ok   - S1-3: no directory escaped the runs root"
  pass=$((pass + 1))
fi

# --- S1-4: --resume must not read files outside the run dir -------------------
# Attacker crafts a log.jsonl whose chain_file is an absolute path to a secret.
# Resume must refuse to restore that file's content.
mkdir -p s14-run
echo "TOP-SECRET-S14" > s14-secret.txt
cat > s14-run/meta.json <<'JSON'
{"sfh_version":"0.9.0","flow":"","flow_fingerprint":"abc","name":"s14","started_utc":"20250101-000000","os":"linux","vars":{},"tools":{},"resumed":false}
JSON
SECRET_ABS="$(cd "$(dirname s14-secret.txt)" && pwd)/$(basename s14-secret.txt)"
cat > s14-run/log.jsonl <<JSON
{"ts":"20250101-000000","event":"run_start","sfh_version":"0.9.0","resumed":false,"flow_fingerprint":"abc"}
{"ts":"20250101-000001","event":"step_start","step":"one","visit":1,"cmd":"echo hi"}
{"ts":"20250101-000002","event":"step_end","step":"one","parent":null,"visit":1,"exit":0,"timed_out":false,"interrupted":false,"attempts":1,"dur_ms":10,"output_chars":2,"input_tokens":null,"output_tokens":null,"cost_usd":null,"tool":null,"chain_file":"$SECRET_ABS","out_file":"$SECRET_ABS","cmd":"echo hi","session":null}
JSON
cat > s14.yaml <<'YAML'
name: s14
steps:
  - id: one
    cmd: ["echo", "hi"]
  - id: two
    cmd: ["echo", "leaked={{steps.one.output}}"]
YAML
"$SFH" run s14.yaml --resume s14-run --force-resume -q > s14.out 2>s14.err
if grep -qF "TOP-SECRET-S14" s14.out; then
  echo "FAIL - S1-4: resume restored content from outside the run dir"
  fail=$((fail + 1))
else
  echo "ok   - S1-4: resume does not read files outside the run dir"
  pass=$((pass + 1))
fi

# --- S2-1: cursor has two tiers; write is refused, not silently full ----------
cat > s21.yaml <<'YAML'
name: s21
steps:
  - id: a
    tool: cursor
    access: write
    prompt: "x"
YAML
"$SFH" validate s21.yaml > s21.out 2>&1
check "S2-1: cursor access: write is a validation error" 2 $?
contains "S2-1: the error names the two real tiers" "two permission tiers" s21.out
cat > s21-ok.yaml <<'YAML'
name: s21-ok
steps:
  - id: a
    tool: cursor
    access: read
    prompt: "x"
  - id: b
    tool: cursor
    access: full
    prompt: "y"
YAML
"$SFH" validate s21-ok.yaml > s21-ok.out 2>&1
check "S2-1: cursor read and full validate" 0 $?

# --- S2-2: AI steps must declare access; cmd: steps are exempt ----------------
cat > s22.yaml <<'YAML'
name: s22
steps:
  - id: a
    tool: codex
    prompt: "x"
YAML
"$SFH" validate s22.yaml > s22.out 2>&1
check "S2-2: an AI step without access is rejected" 2 $?
contains "S2-2: the error asks for an explicit level" "read, write, or full" s22.out
cat > s22-ok.yaml <<'YAML'
name: s22-ok
profiles:
  p: { tool: codex, access: read }
defaults:
  access: full
steps:
  - id: a
    tool: codex
    access: read
    prompt: "x"
  - id: b
    use: p
    prompt: "y"
  - id: c
    tool: claude
    prompt: "z"
  - id: d
    cmd: ["echo", "cmd steps are exempt"]
YAML
"$SFH" validate s22-ok.yaml > s22-ok.out 2>&1
check "S2-2: step, profile and defaults access all satisfy; cmd exempt" 0 $?

# --- S2-3: permission flags in args: fail closed ------------------------------
# Used to be a warning, and the check missed most levers - including pi's -t,
# which the README itself documented as the way to add Bash to a write step.
s23_case() { # s23_case <name> <tool> <access> <args-json>
  cat > "s23-$1.yaml" <<YAML
name: s23
steps:
  - id: a
    tool: $2
    access: $3
    args: $4
    prompt: "x"
YAML
  "$SFH" validate "s23-$1.yaml" > "s23-$1.out" 2>&1
  check "S2-3: $2 permission flag in args is a validation error" 2 $?
  contains "S2-3: $2 error names the escape hatch" "allow_access_override" "s23-$1.out"
}
s23_case pi-t pi write '["-t", "read,bash,edit,write,grep,find,ls"]'
s23_case codex-s codex write '["-s", "danger-full-access"]'
s23_case codex-cfg codex read '["-c", "sandbox_mode=\"danger-full-access\""]'
s23_case claude-tools claude read '["--allowedTools", "Bash"]'
s23_case opencode-agent opencode read '["--agent", "build"]'
s23_case grok-allow grok write '["--allow", "Bash(ls)"]'
s23_case agy-mode agy read '["--mode", "accept-edits"]'
cat > s23-override.yaml <<'YAML'
name: s23-override
steps:
  - id: a
    tool: pi
    access: write
    allow_access_override: true
    args: ["-t", "read,bash,edit,write,grep,find,ls"]
    prompt: "x"
YAML
"$SFH" validate s23-override.yaml > s23-override.out 2>&1
check "S2-3: allow_access_override is the explicit opt-in" 0 $?
cat > s23-full.yaml <<'YAML'
name: s23-full
steps:
  - id: a
    tool: pi
    access: full
    args: ["--approve"]
    prompt: "x"
YAML
"$SFH" validate s23-full.yaml > s23-full.out 2>&1
check "S2-3: full may carry permission flags" 0 $?
# args accept templates, so the check must run again AFTER rendering: an
# upstream output can inject a flag no load-time check ever saw. `src` really
# runs; `inj` must die before any spawn.
cat > s23-runtime.yaml <<'YAML'
name: s23-runtime
steps:
  - id: src
    cmd: ["echo", "--force"]
  - id: inj
    tool: claude
    access: read
    args: ["{{steps.src.output | trim}}"]
    prompt: "x"
YAML
"$SFH" run s23-runtime.yaml -q > s23-runtime.out 2> s23-runtime.err
check "S2-3: a flag injected by an upstream output is refused before spawn" 1 $?
contains "S2-3: the runtime error names the escape hatch" "allow_access_override" s23-runtime.err

# --- S2-4: a session cannot be resumed at a higher access level ---------------
# bin: "echo" stands in for claude: it exits 0 without calling an AI, and sfh
# pre-assigns the session id itself, so a session gets recorded - with the
# access level it was created under.
cat > s24.yaml <<'YAML'
name: s24
steps:
  - id: low
    tool: claude
    bin: "echo"
    access: read
    prompt: "x"
  - id: high
    tool: claude
    bin: "echo"
    access: full
    continue_from: low
    prompt: "y"
YAML
"$SFH" run s24.yaml --runs-dir s24-runs -q > s24.out 2> s24.err
check "S2-4: resuming a read session at full is refused" 1 $?
contains "S2-4: the error names both levels" "access read" s24.err
contains "S2-4: the error names the escape hatch" "allow_access_override" s24.err
S24_LOG="$(find s24-runs -type f -name 'log.jsonl' -print -quit)"
contains "S2-4: the session records the access it was created under" '"access":"read"' "$S24_LOG"
cat > s24-override.yaml <<'YAML'
name: s24-override
steps:
  - id: low
    tool: claude
    bin: "echo"
    access: read
    prompt: "x"
  - id: high
    tool: claude
    bin: "echo"
    access: full
    allow_access_override: true
    continue_from: low
    prompt: "y"
YAML
"$SFH" run s24-override.yaml -q > s24-override.out 2> s24-override.err
# Judged by the refusal MESSAGE, not the exit code. `bin: "echo"` reports no
# session id, so F-11's "resume unverified" check now fails every one of these
# runs on its own - real claude does report one, so that guard is correct and
# the stub simply cannot satisfy it. An exit-code assertion here would be
# testing F-11, not the access decision this block is about.
if grep -q "refusing to resume" s24-override.err; then
  echo "FAIL - S2-4: allow_access_override did not permit the escalation"
  fail=$((fail + 1))
else
  echo "ok   - S2-4: allow_access_override permits the escalation"
  pass=$((pass + 1))
fi
cat > s24-same.yaml <<'YAML'
name: s24-same
steps:
  - id: low
    tool: claude
    bin: "echo"
    access: read
    prompt: "x"
  - id: again
    tool: claude
    bin: "echo"
    access: read
    continue_from: low
    prompt: "y"
YAML
"$SFH" run s24-same.yaml -q > s24-same.out 2> s24-same.err
if grep -q "refusing to resume" s24-same.err; then
  echo "FAIL - S2-4: resuming at the same access level was refused"
  fail=$((fail + 1))
else
  echo "ok   - S2-4: resuming at the same access level is allowed"
  pass=$((pass + 1))
fi
cat > s24-fork.yaml <<'YAML'
name: s24-fork
steps:
  - id: low
    tool: claude
    bin: "echo"
    access: read
    prompt: "x"
  - id: branch
    tool: claude
    bin: "echo"
    access: full
    fork_from: low
    prompt: "y"
YAML
"$SFH" run s24-fork.yaml -q > s24-fork.out 2> s24-fork.err
check "S2-4: forking a read session at full is refused too" 1 $?

# --- S3-1: template expansion in a string cmd is disabled by default ----------
# The audit's payload contains NO banned metacharacter, yet tar would execute
# code through --checkpoint-action. So the metacharacter blacklist cannot be
# the boundary: expansion itself is refused unless the step opts in.
cat > s31.yaml <<'YAML'
name: s31
steps:
  - id: lister
    cmd: ["echo", "--checkpoint=1 --checkpoint-action=exec='sh payload.sh' harmless.txt"]
  - id: pack
    cmd: "tar -cf backup.tar {{steps.lister.output}}"
YAML
"$SFH" validate s31.yaml > s31.out 2>&1
check "S3-1: template expansion in a string cmd is rejected" 2 $?
contains "S3-1: the error points at the array form" 'cmd: [' s31.out
contains "S3-1: the error names the escape hatch" "unsafe_shell_template" s31.out
"$SFH" run s31.yaml -q > s31-run.out 2> s31-run.err
check "S3-1: run refuses the flow before any step executes" 2 $?

# The same hostile value through the array form is data, not shell syntax: it
# arrives as one argument, intact, and nothing executes it.
cat > s31-argv.yaml <<'YAML'
name: s31-argv
steps:
  - id: lister
    cmd: ["echo", "--checkpoint=1 --checkpoint-action=exec='sh payload.sh' harmless.txt"]
  - id: pack
    cmd: ["printf", "ARG=%s\n", "{{steps.lister.output | trim}}"]
YAML
"$SFH" run s31-argv.yaml -q > s31-argv.out 2>&1
check "S3-1: the array form carries the hostile value as data" 0 $?
contains "S3-1: the value arrives intact as one argument" "ARG=--checkpoint=1 --checkpoint-action=exec='sh payload.sh' harmless.txt" s31-argv.out

# The explicit opt-in expands as before (metacharacter check still applies).
cat > s31-optin.yaml <<'YAML'
name: s31-optin
vars: { f: "harmless.txt" }
steps:
  - id: pack
    unsafe_shell_template: true
    cmd: "echo packing {{vars.f}}"
YAML
"$SFH" run s31-optin.yaml -q > s31-optin.out 2>&1
check "S3-1: unsafe_shell_template allows shell templating" 0 $?
contains "S3-1: the opt-in expands the template" "packing harmless.txt" s31-optin.out

# --- S3-2: only resolved (tool, bin) pairs are ever executed ------------------
# A profile that no step references must never be probed: its bin is data.
cat > evil-probe.sh <<'SH'
#!/bin/sh
touch EVIL-PROBE-RAN
echo evil 1.0
SH
chmod +x evil-probe.sh
cat > s32.yaml <<'YAML'
name: s32
profiles:
  aaa-unused: { tool: codex, bin: "./evil-probe.sh" }
steps:
  - id: real
    tool: codex
    bin: "echo"
    access: read
    prompt: "x"
YAML
rm -f EVIL-PROBE-RAN
"$SFH" run s32.yaml --runs-dir s32-runs -q > s32.out 2>&1
check "S3-2: a flow with an unused hostile profile runs" 0 $?
if [ -f EVIL-PROBE-RAN ]; then
  echo "FAIL - S3-2: the unused profile's bin was executed"
  fail=$((fail + 1))
else
  echo "ok   - S3-2: the unused profile's bin was never executed"
  pass=$((pass + 1))
fi
S32_META="$(dirname "$(find s32-runs -type f -name 'log.jsonl' -print -quit)")/meta.json"
contains "S3-2: provenance records the resolved bin" '"bin": "echo"' "$S32_META"
if grep -qF "evil-probe" "$S32_META"; then
  echo "FAIL - S3-2: the unused profile leaked into provenance"
  fail=$((fail + 1))
else
  echo "ok   - S3-2: the unused profile does not appear in provenance"
  pass=$((pass + 1))
fi
contains "S3-4: meta records the fingerprint algorithm" '"flow_fingerprint_algo": "sha256-nl"' "$S32_META"
# doctor resolves the same way and must not launch the unused bin either.
rm -f EVIL-PROBE-RAN
"$SFH" doctor s32.yaml --runs-dir s32-doctor > s32-doctor.out 2>&1
check "S3-2: doctor probes the resolved bin (echo stands in)" 0 $?
if [ -f EVIL-PROBE-RAN ]; then
  echo "FAIL - S3-2: doctor executed the unused profile's bin"
  fail=$((fail + 1))
else
  echo "ok   - S3-2: doctor never executed the unused profile's bin"
  pass=$((pass + 1))
fi
contains "S3-2: doctor names the resolved program" "echo" s32-doctor.out

# --- S3-3: run artifacts are owner-only and gitignore is verified -------------
# An AI step, not a cmd: step. The prompt file is the most sensitive artifact
# a run writes, and only a step with a prompt: produces one - the old fixture
# asserted 0600 on a file it never created, so `stat` returned nothing and the
# check could not have failed for the right reason either. bin: "echo" stands
# in for the CLI.
cat > s33.yaml <<'YAML'
name: s33
steps:
  - id: a
    tool: claude
    bin: "echo"
    access: read
    prompt: "secret-output"
YAML
"$SFH" run s33.yaml --runs-dir s33-runs -q > s33.out 2>&1
check "S3-3: the permissions fixture runs" 0 $?
S33_DIR="$(dirname "$(find s33-runs -type f -name 'log.jsonl' -print -quit)")"
case "$(uname 2>/dev/null)" in
  MINGW*|MSYS*|CYGWIN*)
    echo "ok   - S3-3: permission bits not enforceable on Windows (skipped)"
    pass=$((pass + 1))
    ;;
  *)
    perm_of() { stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1" 2>/dev/null; }
    check "S3-3: the runs root is 0700" 700 "$(perm_of s33-runs)"
    check "S3-3: the run dir is 0700" 700 "$(perm_of "$S33_DIR")"
    check "S3-3: meta.json is 0600" 600 "$(perm_of "$S33_DIR/meta.json")"
    check "S3-3: the prompt file is 0600" 600 "$(perm_of "$S33_DIR/a.prompt.txt")"
    ;;
esac

# A hostile repo pre-places an EMPTY .gitignore; sfh must not trust it.
mkdir -p s33-empty/runs
: > s33-empty/runs/.gitignore
"$SFH" run s33.yaml --runs-dir s33-empty/runs -q > /dev/null 2> s33-empty.err
check "S3-3: a run against a pre-existing empty .gitignore works" 0 $?
contains "S3-3: sfh warns that it is appending" "appending" s33-empty.err
if grep -qxF '*' s33-empty/runs/.gitignore; then
  echo "ok   - S3-3: sfh appended a star pattern to the empty .gitignore"
  pass=$((pass + 1))
else
  echo "FAIL - S3-3: no star pattern was appended to the empty .gitignore"
  fail=$((fail + 1))
fi
# A star inside a COMMENT does not count as ignoring everything.
mkdir -p s33-comment/runs
printf '# *\n' > s33-comment/runs/.gitignore
"$SFH" run s33.yaml --runs-dir s33-comment/runs -q > /dev/null 2> s33-comment.err
contains "S3-3: a commented-out star is not trusted" "appending" s33-comment.err
# A correct .gitignore is left byte-identical.
mkdir -p s33-ok/runs
printf 'keep-this\n*\n' > s33-ok/runs/.gitignore
cp s33-ok/runs/.gitignore s33-ok-before.txt
"$SFH" run s33.yaml --runs-dir s33-ok/runs -q > /dev/null 2> s33-ok.err
if cmp -s s33-ok-before.txt s33-ok/runs/.gitignore; then
  echo "ok   - S3-3: a correct .gitignore is left untouched"
  pass=$((pass + 1))
else
  echo "FAIL - S3-3: a correct .gitignore was modified"
  fail=$((fail + 1))
fi
if grep -qF "appending" s33-ok.err; then
  echo "FAIL - S3-3: sfh warned about a .gitignore that already ignores everything"
  fail=$((fail + 1))
else
  echo "ok   - S3-3: no warning for a .gitignore that already ignores everything"
  pass=$((pass + 1))
fi

# --- S3-4: the flow fingerprint is SHA-256 ------------------------------------
S34_META="$(dirname "$(find s33-runs -type f -name 'log.jsonl' -print -quit)")/meta.json"
S34_FP="$(sed -n 's/.*"flow_fingerprint": "\([0-9a-f]*\)".*/\1/p' "$S34_META")"
if [ "${#S34_FP}" = "64" ]; then
  echo "ok   - S3-4: the recorded flow fingerprint is 64 hex chars"
  pass=$((pass + 1))
else
  echo "FAIL - S3-4: flow fingerprint is '${S34_FP}' (${#S34_FP} chars, want 64)"
  fail=$((fail + 1))
fi
contains "S3-4: meta names the algorithm" '"flow_fingerprint_algo": "sha256-nl"' "$S34_META"

# --- R-2: a run made by the old FNV fingerprint can still be resumed ----------
# Simulate an old run dir: 16-hex fingerprint, no flow_fingerprint_algo field.
mkdir -p r2-runs
cat > r2.yaml <<'YAML'
name: r2
steps:
  - id: one
    cmd: ["echo", "first"]
  - id: two
    cmd: ["echo", "second"]
YAML
"$SFH" run r2.yaml --runs-dir r2-runs -q > /dev/null 2>&1
R2_DIR="$(dirname "$(find r2-runs -type f -name 'log.jsonl' -print -quit)")"
# Rewrite meta.json to look like an old sfh: FNV fingerprint, no algo field.
R2_FLOW_ABS="$(cd "$(dirname r2.yaml)" && pwd)/$(basename r2.yaml)"
cat > "$R2_DIR/meta.json" <<JSON
{"sfh_version":"0.9.0","flow":"$R2_FLOW_ABS","flow_fingerprint":"0000000000000000","name":"r2","started_utc":"20250101-000000","os":"linux","vars":{},"tools":{},"resumed":false}
JSON
# Truncate the log so the run looks unfinished (remove run_end).
grep -v '"run_end"' "$R2_DIR/log.jsonl" > "$R2_DIR/log.jsonl.tmp"
mv "$R2_DIR/log.jsonl.tmp" "$R2_DIR/log.jsonl"
# Remove the position event so resume has somewhere to go.
grep -v '"position"' "$R2_DIR/log.jsonl" > "$R2_DIR/log.jsonl.tmp"
mv "$R2_DIR/log.jsonl.tmp" "$R2_DIR/log.jsonl"
# The FNV fingerprint won't match (we wrote zeros), so without --force-resume
# it should fail with "different version". But the ALGO detection must work:
# a CORRECT FNV fingerprint should pass. Compute it with a tiny helper.
# Since we can't easily compute FNV in bash, test the negative: the error
# message must say "different version" (proving it compared as FNV, not SHA).
"$SFH" run r2.yaml --resume "$R2_DIR" -q > r2.out 2>r2.err
check "R-2: a wrong FNV fingerprint is detected as a flow change" 2 $?
contains "R-2: the error says different version (not unknown algo)" "different version" r2.err

# The positive half: a run dir whose meta.json carries the CORRECT old FNV
# fingerprint (and no algo field, exactly like sfh 0.9 wrote) must resume
# without --force-resume and run the remaining step with restored outputs.
cat > r2b.yaml <<'YAML'
name: r2b
steps:
  - id: one
    cmd: ["echo", "first"]
  - id: two
    cmd: ["echo", "second:{{steps.one.output | trim}}"]
YAML
mkdir -p r2b-run
R2B_FP="$(fnv1a64 r2b.yaml)"
R2B_FLOW_ABS="$(cd "$(dirname r2b.yaml)" && pwd)/$(basename r2b.yaml)"
cat > r2b-run/meta.json <<JSON
{"sfh_version":"0.9.0","flow":"$R2B_FLOW_ABS","flow_fingerprint":"$R2B_FP","name":"r2b","started_utc":"20250101-000000","os":"linux","vars":{},"tools":{},"resumed":false}
JSON
cat > r2b-run/log.jsonl <<'JSON'
{"ts":"20250101-000000","event":"run_start","sfh_version":"0.9.0","resumed":false,"flow_fingerprint":"x"}
{"ts":"20250101-000001","event":"step_start","step":"one","visit":1,"cmd":"echo first"}
{"ts":"20250101-000002","event":"step_end","step":"one","parent":null,"visit":1,"exit":0,"timed_out":false,"interrupted":false,"attempts":1,"dur_ms":10,"output_chars":5,"input_tokens":null,"output_tokens":null,"cost_usd":null,"tool":null,"chain_file":"one.chain.txt","out_file":"one.out.txt","cmd":"echo first","session":null}
JSON
echo "first" > r2b-run/one.chain.txt
echo "first" > r2b-run/one.out.txt
"$SFH" run r2b.yaml --resume r2b-run -q > r2b.out 2>r2b.err
check "R-2: a correct old FNV fingerprint resumes without --force-resume" 0 $?
contains "R-2: the resumed run saw the restored output of step one" "second:first" r2b.out

# --- resume regression: a crash mid-fan-out is resumable ----------------------
# A fan-out logs no step_start of its own; if the run dies before aggregate_end
# (taskkill /F, power loss) the group_start record is the only trace of where it
# was. A flow whose FIRST step is the fan-out must still have a resume position.
cat > rfan.yaml <<'YAML'
name: rfan
steps:
  - id: fan
    parallel:
      - id: fa
        cmd: ["echo", "FA-A"]
      - id: fb
        cmd: ["echo", "FA-B"]
  - id: last
    cmd: ["echo", "fan-done"]
YAML
mkdir -p rfan-run
cat > rfan-run/meta.json <<'JSON'
{"sfh_version":"0.9.0","flow":"","flow_fingerprint":"x","name":"rfan","started_utc":"20250101-000000","os":"linux","vars":{},"tools":{},"resumed":false}
JSON
cat > rfan-run/log.jsonl <<'JSON'
{"ts":"20250101-000000","event":"run_start","sfh_version":"0.9.0","resumed":false,"flow_fingerprint":"x"}
{"ts":"20250101-000001","event":"group_start","step":"fan","visit":1,"children":2}
{"ts":"20250101-000002","event":"step_end","step":"fa","parent":"fan","visit":1,"exit":0,"timed_out":false,"interrupted":false,"attempts":1,"dur_ms":10,"output_chars":4,"input_tokens":null,"output_tokens":null,"cost_usd":null,"tool":null,"chain_file":"fa.chain.txt","out_file":"fa.out.txt","cmd":"echo FA-A","session":null}
JSON
echo "FA-A" > rfan-run/fa.chain.txt
echo "FA-A" > rfan-run/fa.out.txt
"$SFH" run rfan.yaml --resume rfan-run --force-resume > rfan.out 2>rfan.err
check "a crash before aggregate_end resumes instead of 'cannot tell where'" 0 $?
contains "the unfinished fan-out is reported, not lost" "never recorded an end" rfan.err
contains "the resumed fan-out ran through to the end" "fan-done" rfan.out

# --- F-2: a resumed fan-out must not re-run members that already completed ----
# A crash mid-parallel leaves step_end events for the finished children but no
# aggregate_end. The resume must reuse their recorded outputs instead of running
# them again: the old code rebuilt the whole batch, billing completed children
# twice, opening duplicate sessions, and - with max_total_steps sized for the
# live run - counting the restored child PLUS a full fresh batch against the
# cap, so the resume wedged and could never finish. max_total_steps: 2 is
# exactly "one restored + one fresh": only a resume that skips the completed
# child fits under it.
cat > rskip.yaml <<'YAML'
name: rskip
defaults:
  max_total_steps: 2
steps:
  - id: fan
    parallel:
      - id: fa
        cmd: ["echo", "FA-A"]
      - id: fb
        cmd: ["echo", "FA-B"]
YAML
mkdir -p rskip-run
cat > rskip-run/meta.json <<'JSON'
{"sfh_version":"0.9.0","flow":"","flow_fingerprint":"x","name":"rskip","started_utc":"20250101-000000","os":"linux","vars":{},"tools":{},"resumed":false}
JSON
cat > rskip-run/log.jsonl <<'JSON'
{"ts":"20250101-000000","event":"run_start","sfh_version":"0.9.0","resumed":false,"flow_fingerprint":"x"}
{"ts":"20250101-000001","event":"group_start","step":"fan","visit":1,"children":2}
{"ts":"20250101-000002","event":"step_end","step":"fa","parent":"fan","visit":1,"exit":0,"timed_out":false,"interrupted":false,"attempts":1,"dur_ms":10,"output_chars":4,"input_tokens":null,"output_tokens":null,"cost_usd":null,"tool":null,"chain_file":"fa.chain.txt","out_file":"fa.out.txt","cmd":"echo FA-A","session":null}
JSON
echo "FA-A" > rskip-run/fa.chain.txt
echo "FA-A" > rskip-run/fa.out.txt
"$SFH" run rskip.yaml --resume rskip-run --force-resume > rskip.out 2>rskip.err
check "F-2: resume fits max_total_steps when completed members are skipped" 0 $?
contains "F-2: the completed child is reported restored, not prepared again" "1 restored" rskip.err
# sfh writes log JSON with the keys in ALPHABETICAL order, so "event" is
# followed by "exit", never by "step". A fixed-string needle that assumes the
# two are adjacent matches only the hand-written fixture line above and never
# anything sfh emits - which is exactly how this test could report "not
# executed a second time" while the member was in fact re-running.
FA_ENDS=$(grep -c '"event":"step_end".*"step":"fa"' rskip-run/log.jsonl)
if [ "$FA_ENDS" = "1" ]; then
  echo "ok   - F-2: the completed parallel child was not executed a second time"
  pass=$((pass + 1))
else
  echo "FAIL - F-2: child 'fa' has $FA_ENDS step_end events, expected 1 (the resume re-ran it)"
  fail=$((fail + 1))
fi
contains "F-2: the unfinished child still runs" '"step":"fb"' rskip-run/log.jsonl
contains "F-2: the aggregate reuses the restored child's recorded output" "FA-A" rskip-run/fan.chain.txt
contains "F-2: the aggregate carries the fresh child's output" "FA-B" rskip-run/fan.chain.txt

# --- F-2: the same skip applies to foreach items -------------------------------
# Items log under their "id[i]" label; the one the crash completed must be
# rebuilt from its recorded output, not re-run. Same cap arithmetic as above.
cat > rfeskip.yaml <<'YAML'
name: rfeskip
defaults:
  max_total_steps: 2
steps:
  - id: each
    foreach: { from: "alpha\nbeta" }
    cmd: ["echo", "item-{{item}}"]
YAML
mkdir -p rfeskip-run
cat > rfeskip-run/meta.json <<'JSON'
{"sfh_version":"0.9.0","flow":"","flow_fingerprint":"x","name":"rfeskip","started_utc":"20250101-000000","os":"linux","vars":{},"tools":{},"resumed":false}
JSON
cat > rfeskip-run/log.jsonl <<'JSON'
{"ts":"20250101-000000","event":"run_start","sfh_version":"0.9.0","resumed":false,"flow_fingerprint":"x"}
{"ts":"20250101-000001","event":"foreach_start","step":"each","visit":1,"items":2}
{"ts":"20250101-000002","event":"step_end","step":"each[0]","parent":"each","visit":1,"exit":0,"timed_out":false,"interrupted":false,"attempts":1,"dur_ms":10,"output_chars":10,"input_tokens":null,"output_tokens":null,"cost_usd":null,"tool":null,"chain_file":"each.i0.chain.txt","out_file":"each.i0.out.txt","cmd":"echo item-alpha","session":null}
JSON
echo "item-alpha" > rfeskip-run/each.i0.chain.txt
echo "item-alpha" > rfeskip-run/each.i0.out.txt
"$SFH" run rfeskip.yaml --resume rfeskip-run --force-resume > rfeskip.out 2>rfeskip.err
check "F-2: foreach resume fits max_total_steps when completed items are skipped" 0 $?
contains "F-2: the completed item is reported restored, not prepared again" "1 restored" rfeskip.err
FE_ENDS=$(grep -c '"event":"step_end".*"step":"each\[0\]"' rfeskip-run/log.jsonl)
if [ "$FE_ENDS" = "1" ]; then
  echo "ok   - F-2: the completed foreach item was not executed a second time"
  pass=$((pass + 1))
else
  echo "FAIL - F-2: item 'each[0]' has $FE_ENDS step_end events, expected 1 (the resume re-ran it)"
  fail=$((fail + 1))
fi
contains "F-2: the unfinished item still runs" '"step":"each[1]"' rfeskip-run/log.jsonl
contains "F-2: the foreach aggregate reuses the restored item's output" "item-alpha" rfeskip-run/each.chain.txt
contains "F-2: the foreach aggregate carries the fresh item's output" "item-beta" rfeskip-run/each.chain.txt

# --- F-2: a REAL crash and a REAL resume, not a hand-built log ----------------
# The two tests above assemble their own log.jsonl at visit 1 and resume with
# --force-resume, which is precisely the case the bug did NOT affect - which is
# why they passed while the double billing was still there. A fan-out that
# fails logs an aggregate_end, so a genuine resume re-enters the group at visit
# 2 while its finished members are recorded under visit 1. The lookup missed
# every one of them and the whole batch re-ran, and re-billed.
#
# Members run in the SHARED work dir and append a line each, so the tally is a
# direct count of executions. m3 fails once and arms its own tripwire, so the
# resume must run exactly m3 and reuse m1/m2.
cat > rreal.yaml <<'YAML'
name: rreal
steps:
  - id: fan
    max_parallel: 3
    parallel:
      - id: m1
        cmd: ["sh", "-c", "echo m1 >> rreal-tally.txt; echo out1"]
      - id: m2
        cmd: ["sh", "-c", "echo m2 >> rreal-tally.txt; echo out2"]
      - id: m3
        cmd: ["sh", "-c", "echo m3 >> rreal-tally.txt; if [ -f rreal-trip ]; then echo out3; else touch rreal-trip; exit 7; fi"]
  - id: after
    cmd: ["echo", "fan-finished"]
YAML
"$SFH" run rreal.yaml --runs-dir rreal-runs -q > rreal1.out 2> rreal1.err
check "F-2: the first pass fails on the one bad member" 1 $?
RREAL_DIR="$(ls -d rreal-runs/*/ | head -1 | sed 's:/*$::')"
"$SFH" run rreal.yaml --resume "$RREAL_DIR" --runs-dir rreal-runs -q > rreal2.out 2> rreal2.err
check "F-2: the real resume runs to the end" 0 $?
RM1=$(grep -c '^m1$' rreal-tally.txt)
RM2=$(grep -c '^m2$' rreal-tally.txt)
RM3=$(grep -c '^m3$' rreal-tally.txt)
if [ "$RM1" = "1" ] && [ "$RM2" = "1" ] && [ "$RM3" = "2" ]; then
  echo "ok   - F-2: a real resume reused both finished members and re-ran only the failed one"
  pass=$((pass + 1))
else
  echo "FAIL - F-2: executions were m1=$RM1 m2=$RM2 m3=$RM3, expected 1 1 2 (finished members re-ran = double billing)"
  fail=$((fail + 1))
fi
contains "F-2: the resumed aggregate still carries the reused member's output" "out1" "$RREAL_DIR/fan.v2.chain.txt"
contains "F-2: the flow continued past the fan-out" "fan-finished" rreal2.out

# A harder failure mode: kill sfh while one sibling has finished and another
# is still running. The finished member's step_end must already be durable at
# that instant; otherwise resume has no evidence and executes it (and bills it)
# twice. This is intentionally a real detached process tree, not a cut log.
rm -f rcrash-tally.txt
cat > rcrash.yaml <<'YAML'
api_version: 1
name: rcrash
steps:
  - id: fan-out
    max_parallel: 2
    parallel:
      - id: fast-member
        cmd: ["sh", "-c", "printf 'fast\\n' >> rcrash-tally.txt; echo fast-output"]
      - id: slow-member
        cmd: ["sh", "-c", "printf 'slow-start\\n' >> rcrash-tally.txt; sleep 6; printf 'slow-done\\n' >> rcrash-tally.txt; echo slow-output"]
  - id: after
    cmd: ["echo", "crash-resume-finished"]
YAML
RCRASH_DIR="$("$SFH" run rcrash.yaml --runs-dir rcrash-runs --detach -q 2>rcrash-detach.err)"
RCRASH_FAST_DURABLE=0
for _ in $(seq 1 50); do
  if grep -F '"event":"step_end"' "$RCRASH_DIR/log.jsonl" 2>/dev/null |
      grep -qF '"step":"fast-member"'; then
    RCRASH_FAST_DURABLE=1
    break
  fi
  sleep 0.1
done
check "fan-out crash: a fast member is durable while its sibling is still active" 1 "$RCRASH_FAST_DURABLE"
if grep -F '"event":"step_end"' "$RCRASH_DIR/log.jsonl" 2>/dev/null |
    grep -qF '"step":"slow-member"'; then
  RCRASH_SLOW_STILL_ACTIVE=0
else
  RCRASH_SLOW_STILL_ACTIVE=1
fi
check "fan-out crash: the test really interrupts before the slow member ends" 1 "$RCRASH_SLOW_STILL_ACTIVE"
RCRASH_STATUS_READY=0
for _ in $(seq 1 45); do
  "$SFH" status "$RCRASH_DIR" --json > rcrash-status.json 2>/dev/null || :
  if grep -qF '"fanout_completed": 1' rcrash-status.json &&
      grep -qF '"fanout_total": 2' rcrash-status.json &&
      grep -qF '"slow-member": "running"' rcrash-status.json; then
    RCRASH_STATUS_READY=1
    break
  fi
  sleep 0.1
done
check "fan-out status: total, completed and running member are observable" 1 "$RCRASH_STATUS_READY"
"$SFH" stop "$RCRASH_DIR" > rcrash-stop.out 2> rcrash-stop.err
check "fan-out crash: stop terminates the detached process tree" 0 $?
sleep 1
"$SFH" run rcrash.yaml --resume "$RCRASH_DIR" -q > rcrash-resume.out 2> rcrash-resume.err
check "fan-out crash: resume completes after the hard interruption" 0 $?
RCRASH_FAST_CALLS="$(grep -c '^fast$' rcrash-tally.txt)"
RCRASH_SLOW_STARTS="$(grep -c '^slow-start$' rcrash-tally.txt)"
RCRASH_SLOW_DONE="$(grep -c '^slow-done$' rcrash-tally.txt)"
check "fan-out crash: the durable fast member is not executed twice" 1 "$RCRASH_FAST_CALLS"
check "fan-out crash: only the interrupted member starts again" 2 "$RCRASH_SLOW_STARTS"
check "fan-out crash: the resumed slow member completes once" 1 "$RCRASH_SLOW_DONE"
contains "fan-out crash: the aggregate reuses the fast member output" \
  "fast-output" "$RCRASH_DIR/fan-out.chain.txt"
contains "fan-out crash: the flow continues after reconstruction" \
  "crash-resume-finished" rcrash-resume.out

# --- F-2: the same, for foreach, with a real crash and a real resume ---------
# rfeskip above is the hand-built-log variant and does not exercise the visit
# bump, so foreach had no real-run coverage at all. Items log under "id[i]";
# item `beta` fails once and arms its own tripwire.
cat > rfreal.yaml <<'YAML'
name: rfreal
steps:
  - id: each
    max_parallel: 2
    foreach: { from: "alpha\nbeta" }
    # {{item}} goes in a POSITIONAL argument, never into the script text: the
    # script text is re-parsed by the shell and the guard refuses templates
    # there, which is the whole point. $1 arrives as one word regardless.
    cmd: ['sh', '-c', 'echo "$1" >> rfreal-tally.txt; if [ "$1" = beta ] && [ ! -f rfreal-trip ]; then touch rfreal-trip; exit 7; fi; echo "did-$1"', 'rfreal', '{{item}}']
  - id: after2
    cmd: ["echo", "each-finished"]
YAML
"$SFH" run rfreal.yaml --runs-dir rfreal-runs -q > rfreal1.out 2> rfreal1.err
check "F-2: the first foreach pass fails on the one bad item" 1 $?
RFREAL_DIR="$(ls -d rfreal-runs/*/ | head -1 | sed 's:/*$::')"
"$SFH" run rfreal.yaml --resume "$RFREAL_DIR" --runs-dir rfreal-runs -q > rfreal2.out 2> rfreal2.err
check "F-2: the real foreach resume runs to the end" 0 $?
RFA=$(grep -c '^alpha$' rfreal-tally.txt)
RFB=$(grep -c '^beta$' rfreal-tally.txt)
if [ "$RFA" = "1" ] && [ "$RFB" = "2" ]; then
  echo "ok   - F-2: a real foreach resume reused the finished item and re-ran only the failed one"
  pass=$((pass + 1))
else
  echo "FAIL - F-2: foreach executions were alpha=$RFA beta=$RFB, expected 1 2 (finished items re-ran = double billing)"
  fail=$((fail + 1))
fi
contains "F-2: the resumed foreach aggregate carries the reused item's output" "did-alpha" "$RFREAL_DIR/each.v2.chain.txt"
contains "F-2: the flow continued past the foreach" "each-finished" rfreal2.out

# --- resume regression: fan-out routing matches live (headerless plain) -------
# Live routing tests conditions against the headerless plain concatenation; the
# chain file holds the labeled aggregate with "--- id ---" headers. A resume
# that re-reads the chain would match conditions live never saw and could pick
# a different branch. The plain_file copy keeps resume on the same text.
cat > rplain.yaml <<'YAML'
name: rplain
steps:
  - id: fan
    parallel:
      - id: pa
        cmd: ["echo", "AAA"]
      - id: pb
        cmd: ["echo", "BBB"]
    route:
      - when_contains: "--- pb ---"
        goto: bad
  - id: good
    cmd: ["echo", "took-good"]
    route:
      - goto: end
  - id: bad
    cmd: ["echo", "took-bad"]
YAML
mkdir -p rplain-run
cat > rplain-run/meta.json <<'JSON'
{"sfh_version":"0.9.0","flow":"","flow_fingerprint":"x","name":"rplain","started_utc":"20250101-000000","os":"linux","vars":{},"tools":{},"resumed":false}
JSON
cat > rplain-run/log.jsonl <<'JSON'
{"ts":"20250101-000000","event":"run_start","sfh_version":"0.9.0","resumed":false,"flow_fingerprint":"x"}
{"ts":"20250101-000001","event":"group_start","step":"fan","visit":1,"children":2}
{"ts":"20250101-000002","event":"step_end","step":"pa","parent":"fan","visit":1,"exit":0,"timed_out":false,"interrupted":false,"attempts":1,"dur_ms":10,"output_chars":3,"input_tokens":null,"output_tokens":null,"cost_usd":null,"tool":null,"chain_file":"pa.chain.txt","out_file":"pa.out.txt","cmd":"echo AAA","session":null}
{"ts":"20250101-000003","event":"step_end","step":"pb","parent":"fan","visit":1,"exit":0,"timed_out":false,"interrupted":false,"attempts":1,"dur_ms":10,"output_chars":3,"input_tokens":null,"output_tokens":null,"cost_usd":null,"tool":null,"chain_file":"pb.chain.txt","out_file":"pb.out.txt","cmd":"echo BBB","session":null}
{"ts":"20250101-000004","event":"aggregate_end","step":"fan","visit":1,"failed":false,"exit":0,"chain_file":"fan.chain.txt","out_file":"fan.out.txt","plain_file":"fan.plain.txt"}
JSON
printf 'AAA' > rplain-run/pa.chain.txt
cp rplain-run/pa.chain.txt rplain-run/pa.out.txt
printf 'BBB' > rplain-run/pb.chain.txt
cp rplain-run/pb.chain.txt rplain-run/pb.out.txt
printf -- '--- pa ---\nAAA\n\n--- pb ---\nBBB\n' > rplain-run/fan.chain.txt
cp rplain-run/fan.chain.txt rplain-run/fan.out.txt
printf 'AAA\n\nBBB\n' > rplain-run/fan.plain.txt
"$SFH" run rplain.yaml --resume rplain-run --force-resume -q > rplain.out 2>rplain.err
check "resume re-routes a completed fan-out" 0 $?
contains "resume routed against the headerless plain text, like live" "took-good" rplain.out
if grep -qF "took-bad" rplain.out; then
  echo "FAIL - resume routed against the labeled chain headers"
  fail=$((fail + 1))
else
  echo "ok   - the labeled chain headers did not steer the resumed route"
  pass=$((pass + 1))
fi

# --- resume regression: the original --var values come back -------------------
# Completed steps ran under the original overrides; routing/foreach/prompts
# after the resume must render with the SAME values, not the flow's defaults.
cat > rvars.yaml <<'YAML'
name: rvars
vars:
  greeting: default-val
steps:
  - id: one
    cmd: ["echo", "one"]
  - id: two
    cmd: ["echo", "greet={{vars.greeting}}"]
YAML
for tag in rvars rvars2; do
  mkdir -p "$tag-run"
  cat > "$tag-run/meta.json" <<'JSON'
{"sfh_version":"0.9.0","flow":"","flow_fingerprint":"x","name":"rvars","started_utc":"20250101-000000","os":"linux","vars":{"greeting":"from-meta"},"tools":{},"resumed":false}
JSON
  cat > "$tag-run/log.jsonl" <<'JSON'
{"ts":"20250101-000000","event":"run_start","sfh_version":"0.9.0","resumed":false,"flow_fingerprint":"x"}
{"ts":"20250101-000001","event":"step_start","step":"one","visit":1,"cmd":"echo one"}
{"ts":"20250101-000002","event":"step_end","step":"one","parent":null,"visit":1,"exit":0,"timed_out":false,"interrupted":false,"attempts":1,"dur_ms":10,"output_chars":3,"input_tokens":null,"output_tokens":null,"cost_usd":null,"tool":null,"chain_file":"one.chain.txt","out_file":"one.out.txt","cmd":"echo one","session":null}
JSON
  echo "one" > "$tag-run/one.chain.txt"
  echo "one" > "$tag-run/one.out.txt"
done
"$SFH" run rvars.yaml --resume rvars-run --force-resume -q > rvars.out 2>rvars.err
check "resume restores the recorded vars from meta.json" 0 $?
contains "a resumed prompt renders with the original override, not the default" "greet=from-meta" rvars.out
# An explicit --var on the resume command overrides the recorded value.
"$SFH" run rvars.yaml --var greeting=from-cli --resume rvars2-run --force-resume -q > rvars2.out 2>rvars2.err
check "resume with an explicit --var still works" 0 $?
contains "an explicit --var beats the recorded value" "greet=from-cli" rvars2.out

# The failure hint must repeat this attempt's --var overrides, quoted so the
# command works when pasted back (values may carry spaces since R-6).
cat > rvarhint.yaml <<'YAML'
name: rvarhint
steps:
  - id: bad
    cmd: ["sh", "-c", "exit 1"]
YAML
"$SFH" run rvarhint.yaml --var "greet=hello world" --runs-dir rvarhint-runs -q > /dev/null 2>rvarhint.err
check "the failing fixture run fails" 1 $?
contains "the resume hint repeats the --var override" '--var "greet=hello world"' rvarhint.err
contains "the resume hint names the run dir" "--resume" rvarhint.err

# --- resume regression: an explicit --resume ignores the default runs root ----
# A read-only checkout may resume a run dir on writable storage; that must not
# fail because the DEFAULT .sfh/runs cannot be created in the cwd. Here a plain
# file occupies the .sfh name, so creating the default root is impossible.
mkdir -p ror-cwd ror-run
echo "not a directory" > ror-cwd/.sfh
cat > ror.yaml <<'YAML'
name: ror
steps:
  - id: one
    cmd: ["echo", "one"]
  - id: two
    cmd: ["echo", "ror-done"]
YAML
cat > ror-run/meta.json <<'JSON'
{"sfh_version":"0.9.0","flow":"","flow_fingerprint":"x","name":"ror","started_utc":"20250101-000000","os":"linux","vars":{},"tools":{},"resumed":false}
JSON
cat > ror-run/log.jsonl <<'JSON'
{"ts":"20250101-000000","event":"run_start","sfh_version":"0.9.0","resumed":false,"flow_fingerprint":"x"}
{"ts":"20250101-000001","event":"step_start","step":"one","visit":1,"cmd":"echo one"}
{"ts":"20250101-000002","event":"step_end","step":"one","parent":null,"visit":1,"exit":0,"timed_out":false,"interrupted":false,"attempts":1,"dur_ms":10,"output_chars":3,"input_tokens":null,"output_tokens":null,"cost_usd":null,"tool":null,"chain_file":"one.chain.txt","out_file":"one.out.txt","cmd":"echo one","session":null}
JSON
echo "one" > ror-run/one.chain.txt
echo "one" > ror-run/one.out.txt
(
  cd ror-cwd && "$SFH" run ../ror.yaml --resume ../ror-run --force-resume -q
) > ror.out 2>ror.err
check "an explicit --resume does not need the default runs root" 0 $?
contains "the resumed run executed its remaining step" "ror-done" ror.out

# --- resume regression: compactor leaf count and cost survive resume ----------
# A live run counts the summarizer as one more leaf run and adds its cost the
# moment compact starts; a resume that drops both under-reports steps_done and
# cost, which can push a resumed run past max_total_steps / max_cost_usd that
# the live run would have honoured.
mkdir -p rcomp-run
cat > rcomp.yaml <<'YAML'
name: rcomp
steps:
  - id: one
    cmd: ["echo", "x"]
  - id: two
    cmd: ["echo", "saw={{steps.one.outputs | trim}}"]
YAML
cat > rcomp-run/meta.json <<'JSON'
{"sfh_version":"0.9.0","flow":"","flow_fingerprint":"x","name":"rcomp","started_utc":"20250101-000000","os":"linux","vars":{},"tools":{},"resumed":false}
JSON
cat > rcomp-run/log.jsonl <<'JSON'
{"ts":"20250101-000000","event":"run_start","sfh_version":"0.9.0","resumed":false,"flow_fingerprint":"x"}
{"ts":"20250101-000001","event":"step_start","step":"one","visit":1,"cmd":"echo x"}
{"ts":"20250101-000002","event":"step_end","step":"one","parent":null,"visit":1,"exit":0,"timed_out":false,"interrupted":false,"attempts":1,"dur_ms":10,"output_chars":11,"input_tokens":null,"output_tokens":null,"cost_usd":null,"tool":null,"chain_file":"one.chain.txt","out_file":"one.out.txt","cmd":"echo x","session":null}
{"ts":"20250101-000003","event":"compact_end","step":"one","chars":7,"cost_usd":0.5,"precompact_file":"one.precompact.txt"}
JSON
echo "the summary" > rcomp-run/one.chain.txt
echo "the summary" > rcomp-run/one.out.txt
echo "the original" > rcomp-run/one.precompact.txt
"$SFH" run rcomp.yaml --resume rcomp-run --force-resume > rcomp.out 2>rcomp.err
check "resume after a compaction completes" 0 $?
contains "the summarizer leaf is counted in steps done" "2 steps already done" rcomp.err
contains "the summarizer cost is restored" '$0.5000' rcomp.err
contains "the resumed step saw the pre-compact original" "saw=the original" rcomp.out

# --- R-3: a legacy run (no nonce) can be stopped via process ownership --------
# The run must look LIVE, or status resolves to "dead" and stop returns before
# the ownership check is ever reached - so the victim is a real process this
# test spawned (a pid like 99999 is dead on arrival on Windows and proves
# nothing). The victim is not an sfh process, so the legacy path must warn
# about the older sfh, then refuse on process ownership.
sleep 30 &
R3_VICTIM=$!
R3_WINPID="$(winpid_of "$R3_VICTIM")"
mkdir -p r3-run
cat > r3-run/status.json <<JSON
{
  "state": "running",
  "current_step": "x",
  "started_utc": "20250101-000000",
  "heartbeat_utc": "20250101-000000",
  "steps_done": 0,
  "cost_usd": 0.0,
  "run_dir": "r3-run",
  "flow": "x.yaml",
  "pid": $R3_WINPID,
  "sfh_version": "0.8.0",
  "exit_code": null,
  "emit_step": null,
  "emit_file": null,
  "error": null,
  "unfinished_step": null
}
JSON
# A legacy run dir has log.jsonl but NO sfh-nonce file.
echo '{"ts":"20250101-000000","event":"run_start"}' > r3-run/log.jsonl
"$SFH" stop r3-run > r3.out 2>r3.err
r3_rc=$?
if grep -qF "nonce mismatch" r3.err; then
  echo "FAIL - R-3: a legacy run was refused for missing nonce"
  fail=$((fail + 1))
else
  echo "ok   - R-3: a legacy run is not refused for missing nonce"
  pass=$((pass + 1))
fi
contains "R-3: the legacy warning is printed" "older sfh" r3.err
if [ "$r3_rc" -ne 0 ]; then
  echo "ok   - R-3: a legacy run naming a non-sfh process is not stopped"
  pass=$((pass + 1))
else
  echo "FAIL - R-3: sfh stopped a non-sfh process"
  fail=$((fail + 1))
fi
if kill -0 "$R3_VICTIM" 2>/dev/null; then
  echo "ok   - R-3: the legacy run's process survived"
  pass=$((pass + 1))
else
  echo "FAIL - R-3: the legacy run's process was killed"
  fail=$((fail + 1))
fi
kill "$R3_VICTIM" 2>/dev/null
wait "$R3_VICTIM" 2>/dev/null

# A legacy run dir WITHOUT log.jsonl is a bare status.json - must be refused
# as unrecognisable even when it names a live process.
sleep 30 &
R3B_VICTIM=$!
R3B_WINPID="$(winpid_of "$R3B_VICTIM")"
mkdir -p r3-bare
sed "s/\"pid\": $R3_WINPID/\"pid\": $R3B_WINPID/" r3-run/status.json > r3-bare/status.json
"$SFH" stop r3-bare > r3-bare.out 2>r3-bare.err
r3b_rc=$?
if [ "$r3b_rc" -ne 0 ] && grep -qF "not recognisable" r3-bare.err; then
  echo "ok   - R-3: a bare status.json (no log, no nonce) is refused"
  pass=$((pass + 1))
else
  echo "FAIL - R-3: a bare status.json was not refused"
  fail=$((fail + 1))
fi
kill "$R3B_VICTIM" 2>/dev/null
wait "$R3B_VICTIM" 2>/dev/null

# The positive half: a REAL detached run of this very sfh whose nonce records
# are removed is exactly the legacy format. The checks above prove a non-sfh
# process is refused on this path; here the process genuinely is sfh, so stop
# must succeed on process ownership alone.
cat > r3c-long.yaml <<'YAML'
name: r3c-long
steps:
  - id: think
    cmd: ["sh", "-c", "sleep 60"]
YAML
R3C_DIR="$("$SFH" run r3c-long.yaml --detach -q 2>/dev/null)"
sleep 2
r3c_rc=1
# The heartbeat rewrites status.json every 3s and puts the nonce field back,
# so strip both records and stop at once; retry if a heartbeat lands between.
for _ in 1 2 3; do
  rm -f "$R3C_DIR/sfh-nonce"
  # Dropping a line can leave a dangling comma on the new last key, and which
  # key that is depends on the (alphabetical) field order - naming one of them
  # here broke the moment status.json grew a field later in the alphabet.
  # Strip the comma from whatever line now sits above the closing brace.
  grep -v '"nonce":' "$R3C_DIR/status.json" |
    awk '{l[NR] = $0} END {for (i = 1; i <= NR; i++) {if (i == NR - 1) sub(/,[ \t]*$/, "", l[i]); print l[i]}}' \
      > "$R3C_DIR/status.json.tmp"
  mv "$R3C_DIR/status.json.tmp" "$R3C_DIR/status.json"
  "$SFH" stop "$R3C_DIR" > r3c.out 2>r3c.err
  r3c_rc=$?
  grep -qF "nonce present on only one side" r3c.err || break
done
check "R-3: a real nonce-less sfh run is stopped successfully" 0 "$r3c_rc"
contains "R-3: the legacy warning is printed for a real sfh run" "older sfh" r3c.err
contains "R-3: the kill of the real sfh run is reported" "killed pid" r3c.out
not_contains "R-3: the successful stop report is not split" "killed pid" r3c.err

# --- R-5: narrowing args pass; only widening args are refused -----------------
cat > r5-narrow.yaml <<'YAML'
name: r5-narrow
steps:
  - id: safe
    tool: codex
    access: read
    args: ["-c", "sandbox_mode=read-only"]
    prompt: hi
YAML
"$SFH" validate r5-narrow.yaml > r5-narrow.out 2>&1
check "R-5: a narrowing arg (sandbox_mode=read-only on read) passes" 0 $?
cat > r5-narrow2.yaml <<'YAML'
name: r5-narrow2
steps:
  - id: safe
    tool: codex
    access: write
    args: ["-c", "sandbox_mode=read-only"]
    prompt: hi
YAML
"$SFH" validate r5-narrow2.yaml > r5-narrow2.out 2>&1
check "R-5: a narrowing arg (read-only on write) passes" 0 $?
cat > r5-wide.yaml <<'YAML'
name: r5-wide
steps:
  - id: bad
    tool: codex
    access: read
    args: ["-c", "sandbox_mode=danger-full-access"]
    prompt: hi
YAML
"$SFH" validate r5-wide.yaml > r5-wide.out 2>&1
check "R-5: a widening arg (danger-full-access on read) is refused" 2 $?
# The error must NOT suggest "access: full" (wrong direction).
if grep -qF "access: full" r5-wide.out; then
  echo "FAIL - R-5: the error suggests access: full (wrong direction)"
  fail=$((fail + 1))
else
  echo "ok   - R-5: the error does not suggest access: full"
  pass=$((pass + 1))
fi
contains "R-5: the error names the escape hatch" "allow_access_override" r5-wide.out

# --- R-6: safe flow names pass; validate and run agree ------------------------
cat > r6.yaml <<'YAML'
name: "研究 2026.07"
steps:
  - id: ok
    cmd: ["echo", "ok"]
YAML
"$SFH" validate r6.yaml > r6-val.out 2>&1
check "R-6: validate accepts a Unicode name with spaces and dots" 0 $?
"$SFH" run r6.yaml --dry-run --runs-dir r6-runs > r6-run.out 2>r6-run.err
check "R-6: run --dry-run accepts the same name (validate and run agree)" 0 $?
# A name with a path separator is refused by BOTH.
cat > r6-bad.yaml <<'YAML'
name: "a/b"
steps:
  - id: ok
    cmd: ["echo", "ok"]
YAML
"$SFH" validate r6-bad.yaml > r6-bad-val.out 2>&1
check "R-6: validate refuses a name with a path separator" 2 $?
"$SFH" run r6-bad.yaml --dry-run --runs-dir r6-runs > /dev/null 2>r6-bad-run.err
check "R-6: run refuses the same name (validate and run agree)" 2 $?
# A name that is exactly ".." is refused.
cat > r6-dotdot.yaml <<'YAML'
name: ".."
steps:
  - id: ok
    cmd: ["echo", "ok"]
YAML
"$SFH" validate r6-dotdot.yaml > r6-dotdot.out 2>&1
check "R-6: the reserved name '..' is refused" 2 $?

# A real run under a spaced name: the command hints status prints must quote
# the run dir, or the suggested command falls apart when pasted back.
cat > r6live.yaml <<'YAML'
name: "live 研究"
steps:
  - id: a
    cmd: ["echo", "ok"]
YAML
"$SFH" run r6live.yaml --runs-dir r6live-runs -q > /dev/null 2>&1
check "R-6: a spaced-name run completes" 0 $?
R6L_DIR="$(dirname "$(find r6live-runs -type f -name 'log.jsonl' -print -quit)")"
"$SFH" status "$R6L_DIR" > r6l-status.out 2>r6l-status.err
contains "R-6: the status hint quotes a run dir with spaces" 'sfh wait "' r6l-status.out
not_contains "R-6: successful human status stays in one stream" 'sfh wait "' r6l-status.err

# --- R-7: an existing --runs-dir keeps its permissions ------------------------
case "$(uname 2>/dev/null)" in
  MINGW*|MSYS*|CYGWIN*)
    echo "ok   - R-7: permission bits not enforceable on Windows (skipped)"
    pass=$((pass + 1))
    ;;
  *)
    mkdir -p r7-shared
    chmod 0770 r7-shared
    cat > r7.yaml <<'YAML'
name: r7
steps:
  - id: a
    cmd: ["echo", "hi"]
YAML
    "$SFH" run r7.yaml --runs-dir r7-shared -q > /dev/null 2>&1
    r7_perm="$(stat -c '%a' r7-shared 2>/dev/null || stat -f '%Lp' r7-shared 2>/dev/null)"
    check "R-7: an existing 0770 --runs-dir keeps its permissions" 770 "$r7_perm"
    # But a NEW dir sfh creates inside it gets 0700.
    R7_RUN="$(dirname "$(find r7-shared -type f -name 'log.jsonl' -print -quit)")"
    r7_run_perm="$(stat -c '%a' "$R7_RUN" 2>/dev/null || stat -f '%Lp' "$R7_RUN" 2>/dev/null)"
    check "R-7: the run dir sfh created inside is 0700" 700 "$r7_run_perm"
    ;;
esac

# --- R-1: sfh stop verifies process ownership on THIS OS ----------------------
# macOS has no /proc, so the ownership question is answered per-OS (proc_pidpath
# on macOS, /proc/<pid>/exe on Linux, QueryFullProcessImageNameW on Windows).
# A successful stop of a real detached run proves the check works on the OS the
# suite runs on: it can only succeed when pid_is_sfh recognised our own binary.
cat > r1-long.yaml <<'YAML'
name: r1-long
steps:
  - id: think
    cmd: ["sh", "-c", "sleep 60"]
YAML
R1_DIR="$("$SFH" run r1-long.yaml --detach -q 2>/dev/null)"
sleep 2
"$SFH" stop "$R1_DIR" > r1.out 2>r1.err
check "R-1: sfh stop succeeds on this OS (ownership verified)" 0 $?
contains "R-1: the kill is reported" "killed pid" r1.out
not_contains "R-1: the successful stop report is not split" "killed pid" r1.err
"$SFH" status "$R1_DIR" > r1-status.out 2>&1
contains "R-1: the run is recorded as stopped" "stopped" r1-status.out

# --- R-4: one nonce per attempt; status.json and sfh-nonce never disagree -----
# The parent mints the nonce and the detached child inherits it (SFH_NONCE), so
# no observer can ever see the two files disagree. Sample them hard right after
# the detach, then require a stop landing at once not to be refused.
R4_DIR="$("$SFH" run r1-long.yaml --detach -q 2>/dev/null)"
r4_bad=0
for _ in $(seq 1 150); do
  [ -f "$R4_DIR/sfh-nonce" ] && [ -f "$R4_DIR/status.json" ] || continue
  n_file="$(awk '{print $NF}' "$R4_DIR/sfh-nonce" 2>/dev/null)"
  p_file="$(awk '{print $1}' "$R4_DIR/sfh-nonce" 2>/dev/null)"
  n_status="$(sed -n 's/.*"nonce": *"\([0-9a-f]*\)".*/\1/p' "$R4_DIR/status.json" 2>/dev/null)"
  p_status="$(sed -n 's/.*"pid": *\([0-9]*\).*/\1/p' "$R4_DIR/status.json" 2>/dev/null)"
  [ -z "$n_status" ] && continue
  if [ "$n_file" != "$n_status" ] || [ "$p_file" != "$p_status" ]; then
    r4_bad=1
  fi
done
if [ "$r4_bad" -eq 0 ]; then
  echo "ok   - R-4: status.json and sfh-nonce never disagree after detach"
  pass=$((pass + 1))
else
  echo "FAIL - R-4: status.json and sfh-nonce disagreed (nonce race)"
  fail=$((fail + 1))
fi
R4B_DIR="$("$SFH" run r1-long.yaml --detach -q 2>/dev/null)"
"$SFH" stop "$R4B_DIR" > r4b.out 2>r4b.err
check "R-4: sfh stop right after detach is not refused" 0 $?
if grep -qF "nonce" r4b.err; then
  echo "FAIL - R-4: an immediate stop complained about the nonce"
  fail=$((fail + 1))
else
  echo "ok   - R-4: an immediate stop raises no nonce complaint"
  pass=$((pass + 1))
fi

# A stale heartbeat with a LIVE pid is the wedged process (or one that just
# came back from suspend) that most needs stopping. status resolves it to
# "dead", but stop must not take that as "already done": it has to verify
# ownership and kill. SIGSTOP freezes the run so no heartbeat can refresh the
# file; Windows has no equivalent, so the check is skipped there.
case "$(uname 2>/dev/null)" in
  MINGW*|MSYS*|CYGWIN*)
    echo "ok   - stale-heartbeat stop of a live pid (skipped: no SIGSTOP on Windows)"
    pass=$((pass + 1))
    ;;
  *)
    cat > rh-long.yaml <<'YAML'
name: rh-long
steps:
  - id: think
    cmd: ["sh", "-c", "sleep 60"]
YAML
    RH_DIR="$("$SFH" run rh-long.yaml --detach -q 2>/dev/null)"
    sleep 2
    RH_PID="$(sed -n 's/.*"pid": *\([0-9]*\).*/\1/p' "$RH_DIR/status.json")"
    kill -STOP "$RH_PID"
    touch -t 202501010000 "$RH_DIR/status.json"
    "$SFH" stop "$RH_DIR" > rh.out 2>rh.err
    check "sfh stop works on a stale-heartbeat run whose pid is alive" 0 $?
    contains "stop says why it is acting on a 'dead' run" "but process" rh.err
    contains "the wedged run's kill is reported" "killed pid" rh.out
    not_contains "the wedged run's successful stop report is not split" "killed pid" rh.err
    kill -CONT "$RH_PID" 2>/dev/null
    kill -9 "$RH_PID" 2>/dev/null
    ;;
esac

# --- S1-4 extended: out_file and precompact_file containment ------------------
# out_file pointing outside the run dir must fail the resume.
mkdir -p s14b-run
echo "TOP-SECRET-S14B" > s14b-secret.txt
S14B_ABS="$(cd "$(dirname s14b-secret.txt)" && pwd)/$(basename s14b-secret.txt)"
cat > s14b-run/meta.json <<'JSON'
{"sfh_version":"0.9.0","flow":"","flow_fingerprint":"abc","name":"s14b","started_utc":"20250101-000000","os":"linux","vars":{},"tools":{},"resumed":false}
JSON
cat > s14b-run/log.jsonl <<JSON
{"ts":"20250101-000000","event":"run_start","sfh_version":"0.9.0","resumed":false,"flow_fingerprint":"abc"}
{"ts":"20250101-000001","event":"step_start","step":"one","visit":1,"cmd":"echo hi"}
{"ts":"20250101-000002","event":"step_end","step":"one","parent":null,"visit":1,"exit":0,"timed_out":false,"interrupted":false,"attempts":1,"dur_ms":10,"output_chars":2,"input_tokens":null,"output_tokens":null,"cost_usd":null,"tool":null,"chain_file":"one.chain.txt","out_file":"$S14B_ABS","cmd":"echo hi","session":null}
JSON
echo "chain" > s14b-run/one.chain.txt
cat > s14b.yaml <<'YAML'
name: s14b
steps:
  - id: one
    cmd: ["echo", "hi"]
  - id: two
    cmd: ["echo", "leaked={{steps.one.output}}"]
YAML
"$SFH" run s14b.yaml --resume s14b-run --force-resume -q > s14b.out 2>s14b.err
s14b_rc=$?
if [ "$s14b_rc" -ne 0 ] && ! grep -qF "TOP-SECRET-S14B" s14b.out; then
  echo "ok   - S1-4: an absolute out_file outside the run dir fails the resume"
  pass=$((pass + 1))
else
  echo "FAIL - S1-4: an absolute out_file was not refused"
  fail=$((fail + 1))
fi

# precompact_file pointing outside must also fail.
mkdir -p s14c-run
echo "TOP-SECRET-S14C" > s14c-secret.txt
S14C_ABS="$(cd "$(dirname s14c-secret.txt)" && pwd)/$(basename s14c-secret.txt)"
cat > s14c-run/meta.json <<'JSON'
{"sfh_version":"0.9.0","flow":"","flow_fingerprint":"abc","name":"s14c","started_utc":"20250101-000000","os":"linux","vars":{},"tools":{},"resumed":false}
JSON
cat > s14c-run/log.jsonl <<JSON
{"ts":"20250101-000000","event":"run_start","sfh_version":"0.9.0","resumed":false,"flow_fingerprint":"abc"}
{"ts":"20250101-000001","event":"step_start","step":"one","visit":1,"cmd":"echo hi"}
{"ts":"20250101-000002","event":"step_end","step":"one","parent":null,"visit":1,"exit":0,"timed_out":false,"interrupted":false,"attempts":1,"dur_ms":10,"output_chars":2,"input_tokens":null,"output_tokens":null,"cost_usd":null,"tool":null,"chain_file":"one.chain.txt","out_file":"one.out.txt","cmd":"echo hi","session":null}
{"ts":"20250101-000003","event":"compact_end","step":"one","chars":5,"cost_usd":0.0,"precompact_file":"$S14C_ABS"}
JSON
echo "chain" > s14c-run/one.chain.txt
cat > s14c.yaml <<'YAML'
name: s14c
steps:
  - id: one
    cmd: ["echo", "hi"]
  - id: two
    cmd: ["echo", "leaked={{steps.one.output}}"]
YAML
"$SFH" run s14c.yaml --resume s14c-run --force-resume -q > s14c.out 2>s14c.err
s14c_rc=$?
if [ "$s14c_rc" -ne 0 ] && ! grep -qF "TOP-SECRET-S14C" s14c.out; then
  echo "ok   - S1-4: an absolute precompact_file outside the run dir fails the resume"
  pass=$((pass + 1))
else
  echo "FAIL - S1-4: an absolute precompact_file was not refused"
  fail=$((fail + 1))
fi

# A symlink inside the run dir pointing outside must also be caught. Needs
# real symlinks (see S1-1): a copy would be a legitimate run-dir file.
if have_symlinks; then
  mkdir -p s14d-run
  echo "TOP-SECRET-S14D" > s14d-secret.txt
  cat > s14d-run/meta.json <<'JSON'
{"sfh_version":"0.9.0","flow":"","flow_fingerprint":"abc","name":"s14d","started_utc":"20250101-000000","os":"linux","vars":{},"tools":{},"resumed":false}
JSON
  MSYS=winsymlinks:nativestrict ln -sf "$(pwd)/s14d-secret.txt" s14d-run/one.chain.txt
  cat > s14d-run/log.jsonl <<'JSON'
{"ts":"20250101-000000","event":"run_start","sfh_version":"0.9.0","resumed":false,"flow_fingerprint":"abc"}
{"ts":"20250101-000001","event":"step_start","step":"one","visit":1,"cmd":"echo hi"}
{"ts":"20250101-000002","event":"step_end","step":"one","parent":null,"visit":1,"exit":0,"timed_out":false,"interrupted":false,"attempts":1,"dur_ms":10,"output_chars":2,"input_tokens":null,"output_tokens":null,"cost_usd":null,"tool":null,"chain_file":"one.chain.txt","out_file":"one.out.txt","cmd":"echo hi","session":null}
JSON
  cat > s14d.yaml <<'YAML'
name: s14d
steps:
  - id: one
    cmd: ["echo", "hi"]
  - id: two
    cmd: ["echo", "leaked={{steps.one.output}}"]
YAML
  "$SFH" run s14d.yaml --resume s14d-run --force-resume -q > s14d.out 2>s14d.err
  s14d_rc=$?
  if [ "$s14d_rc" -ne 0 ] && ! grep -qF "TOP-SECRET-S14D" s14d.out; then
    echo "ok   - S1-4: a symlink chain_file pointing outside fails the resume"
    pass=$((pass + 1))
  else
    echo "FAIL - S1-4: a symlink chain_file was not refused"
    fail=$((fail + 1))
  fi
else
  echo "ok   - S1-4: symlink chain_file (skipped: no native symlink support)"
  pass=$((pass + 1))
fi

# --- S2-4 extended: missing recorded access is fail-closed --------------------
# A run dir whose log has no "access" field in the session must refuse a
# higher-access resume unless allow_access_override is set.
cat > s24-missing.yaml <<'YAML'
name: s24-missing
steps:
  - id: low
    tool: claude
    bin: "echo"
    access: read
    prompt: "x"
  - id: high
    tool: claude
    bin: "echo"
    access: full
    continue_from: low
    prompt: "y"
YAML
"$SFH" run s24-missing.yaml --runs-dir s24m-runs -q > /dev/null 2>&1
S24M_DIR="$(dirname "$(find s24m-runs -type f -name 'log.jsonl' -print -quit)")"
# Strip the "access" field from the session in the log to simulate an old run.
sed 's/"access":"read",//' "$S24M_DIR/log.jsonl" | sed 's/,"access":"read"//' > "$S24M_DIR/log.jsonl.tmp"
mv "$S24M_DIR/log.jsonl.tmp" "$S24M_DIR/log.jsonl"
# Remove run_end and position so it looks resumable.
grep -v '"run_end"\|"position"' "$S24M_DIR/log.jsonl" > "$S24M_DIR/log.jsonl.tmp"
mv "$S24M_DIR/log.jsonl.tmp" "$S24M_DIR/log.jsonl"
"$SFH" run s24-missing.yaml --resume "$S24M_DIR" --force-resume -q > s24m.out 2>s24m.err
s24m_rc=$?
if [ "$s24m_rc" -ne 0 ]; then
  echo "ok   - S2-4: a session with no recorded access is fail-closed on resume"
  pass=$((pass + 1))
else
  echo "FAIL - S2-4: a session with no recorded access was resumed at full"
  fail=$((fail + 1))
fi
contains "S2-4: the error names the missing access" "no recorded access level" s24m.err

# --- S3-3 extended: a read-only .gitignore fails the run ----------------------
case "$(uname 2>/dev/null)" in
  MINGW*|MSYS*|CYGWIN*)
    echo "ok   - S3-3: read-only .gitignore test skipped on Windows"
    pass=$((pass + 1))
    ;;
  *)
    # Root ignores the write bit, so "cannot be fixed" is not reachable as
    # root and this would report a failure that says nothing about sfh. CI runs
    # as an ordinary user; a container or WSL shell often does not.
    if [ "$(id -u 2>/dev/null || echo 1)" = "0" ]; then
      echo "ok   - S3-3: read-only .gitignore test skipped (running as root)"
      pass=$((pass + 1))
      false
    else
    mkdir -p s33-ro/runs
    printf 'keep\n' > s33-ro/runs/.gitignore
    chmod 0444 s33-ro/runs/.gitignore
    "$SFH" run s33.yaml --runs-dir s33-ro/runs -q > /dev/null 2>s33-ro.err
    s33ro_rc=$?
    chmod 0644 s33-ro/runs/.gitignore
    if [ "$s33ro_rc" -ne 0 ]; then
      echo "ok   - S3-3: a read-only .gitignore that cannot be fixed fails the run"
      pass=$((pass + 1))
    else
      echo "FAIL - S3-3: a read-only .gitignore was silently ignored"
      fail=$((fail + 1))
    fi
    fi
    ;;
esac

# --- F2: the idle clock -------------------------------------------------------
# B-12 was 112 minutes of silence in which every external signal - pid alive,
# heartbeat fresh, state "running" - reported a healthy run. sfh reads the
# child's chunks as they arrive and used to throw the arrival times away. These
# tests are about the second clock: not "how long has this taken" but "how long
# since anything was said".

# 1. Speaks once, then goes quiet for good. A timeout at the end of that is a
#    hang, and a hang is transient - a retry costs nothing and often works.
cat > f2-hang.yaml <<'YAML'
name: f2-hang
defaults:
  hang_after_sec: 1
steps:
  - id: wedge
    cmd: ["sh", "-c", "echo FIRST-AND-LAST; sleep 30"]
    timeout_sec: 4
    retry: { max: 1, backoff_sec: 1 }
    retry_on: transient
    on_error: continue
YAML
"$SFH" run f2-hang.yaml --runs-dir f2-hang-runs -q > /dev/null 2>f2-hang.err
F2_HANG_DIR="$(dirname "$(find f2-hang-runs -type f -name 'log.jsonl' -print -quit)")"
if [ -f "$F2_HANG_DIR/wedge.a2.out.txt" ]; then
  echo "ok   - a timeout after long silence is retried as a hang"
  pass=$((pass + 1))
else
  echo "FAIL - a hung step was not retried (no wedge.a2.out.txt in $F2_HANG_DIR)"
  ls "$F2_HANG_DIR"
  fail=$((fail + 1))
fi
contains "step_end records idle_ms" '"idle_ms":' "$F2_HANG_DIR/log.jsonl"
F2_HANG_IDLE="$(sed -n 's/.*"event":"step_end".*"idle_ms":\([0-9]*\).*/\1/p' "$F2_HANG_DIR/log.jsonl" | head -1)"
if [ -n "$F2_HANG_IDLE" ] && [ "$F2_HANG_IDLE" -ge 2000 ]; then
  echo "ok   - the recorded idle_ms covers the silent stretch (${F2_HANG_IDLE}ms)"
  pass=$((pass + 1))
else
  echo "FAIL - idle_ms was '$F2_HANG_IDLE'ms after ~4s of silence"
  fail=$((fail + 1))
fi

# 2. Talks the whole way through and still runs out of clock. That is overrun,
#    not a hang: retrying it would just burn the same budget again.
#
#    hang_after_sec has to be SHORTER than the timeout or this test proves
#    nothing: with a threshold above the step's whole lifetime, an idle clock
#    that had failed completely (idle_ms == elapsed, which is what a stdout
#    reader that stopped touching the clock produces) would still be under it
#    and the "no retry" branch would pass for the wrong reason. At 2s against a
#    5s timeout, that broken clock reads 5000ms, crosses the threshold, and the
#    step comes back retried. The idle_ms assertion below says the same thing
#    directly.
cat > f2-chatty.yaml <<'YAML'
name: f2-chatty
defaults:
  hang_after_sec: 2
steps:
  - id: chatty
    cmd: ["sh", "-c", "i=0; while [ $i -lt 60 ]; do echo tick; sleep 1; i=$((i+1)); done"]
    timeout_sec: 5
    retry: { max: 1, backoff_sec: 1 }
    retry_on: transient
    on_error: continue
YAML
"$SFH" run f2-chatty.yaml --runs-dir f2-chatty-runs -q > /dev/null 2>f2-chatty.err
F2_CHATTY_DIR="$(dirname "$(find f2-chatty-runs -type f -name 'log.jsonl' -print -quit)")"
if [ -f "$F2_CHATTY_DIR/chatty.a2.out.txt" ]; then
  echo "FAIL - a step that never stopped talking was retried as a hang"
  fail=$((fail + 1))
else
  echo "ok   - a timeout with steady output is overrun, not a hang, and is not retried"
  pass=$((pass + 1))
fi
F2_CHATTY_IDLE="$(sed -n 's/.*"event":"step_end".*"idle_ms":\([0-9]*\).*/\1/p' "$F2_CHATTY_DIR/log.jsonl" | head -1)"
if [ -n "$F2_CHATTY_IDLE" ] && [ "$F2_CHATTY_IDLE" -lt 2000 ]; then
  echo "ok   - the idle clock followed stdout, so the timeout is overrun (${F2_CHATTY_IDLE}ms)"
  pass=$((pass + 1))
else
  echo "FAIL - idle_ms was '$F2_CHATTY_IDLE'ms although stdout spoke every second"
  fail=$((fail + 1))
fi

# 2b. The same threshold written on the STEP instead of under defaults:. Nothing
#     else in the suite exercises that override, so a refactor that dropped the
#     step half of `step.hang_after_sec.or(defaults)` would leave every test
#     green while a step that says "I go quiet for a long time, do not call it a
#     hang" silently got the 300s default instead.
cat > f2-step.yaml <<'YAML'
name: f2-step
steps:
  - id: wedge
    cmd: ["sh", "-c", "echo FIRST-AND-LAST; sleep 30"]
    hang_after_sec: 1
    timeout_sec: 4
    retry: { max: 1, backoff_sec: 1 }
    retry_on: transient
    on_error: continue
YAML
"$SFH" run f2-step.yaml --runs-dir f2-step-runs -q > /dev/null 2>f2-step.err
F2_STEP_DIR="$(dirname "$(find f2-step-runs -type f -name 'log.jsonl' -print -quit)")"
if [ -f "$F2_STEP_DIR/wedge.a2.out.txt" ]; then
  echo "ok   - hang_after_sec on the step alone is honoured (retried as a hang)"
  pass=$((pass + 1))
else
  echo "FAIL - a step-level hang_after_sec was ignored (no wedge.a2.out.txt)"
  ls "$F2_STEP_DIR"
  fail=$((fail + 1))
fi

# 3. The decisive one: progress on stderr ONLY. Several CLIs report progress
#    there and nowhere else, so a clock fed by stdout alone would file every one
#    of their timeouts as a hang.
cat > f2-stderr.yaml <<'YAML'
name: f2-stderr
defaults:
  hang_after_sec: 2
steps:
  - id: noisy
    cmd: ["sh", "-c", "i=0; while [ $i -lt 60 ]; do echo tick >&2; sleep 1; i=$((i+1)); done"]
    timeout_sec: 5
    retry: { max: 1, backoff_sec: 1 }
    retry_on: transient
    on_error: continue
YAML
"$SFH" run f2-stderr.yaml --runs-dir f2-stderr-runs -q > /dev/null 2>f2-stderr.err
F2_ERR_DIR="$(dirname "$(find f2-stderr-runs -type f -name 'log.jsonl' -print -quit)")"
if [ -f "$F2_ERR_DIR/noisy.a2.out.txt" ]; then
  echo "FAIL - a step that reported progress on stderr was treated as hung"
  fail=$((fail + 1))
else
  echo "ok   - stderr output keeps the idle clock alive (no hang retry)"
  pass=$((pass + 1))
fi
F2_ERR_IDLE="$(sed -n 's/.*"event":"step_end".*"idle_ms":\([0-9]*\).*/\1/p' "$F2_ERR_DIR/log.jsonl" | head -1)"
if [ -n "$F2_ERR_IDLE" ] && [ "$F2_ERR_IDLE" -lt 2000 ]; then
  echo "ok   - idle_ms follows stderr chunks too (${F2_ERR_IDLE}ms after a 5s step)"
  pass=$((pass + 1))
else
  echo "FAIL - idle_ms was '$F2_ERR_IDLE'ms although stderr spoke every second"
  fail=$((fail + 1))
fi

# 4. The same two clocks, live, for whoever is polling from outside.
cat > f2-status.yaml <<'YAML'
name: f2-status
steps:
  - id: slow
    cmd: ["sh", "-c", "echo HELLO; sleep 12"]
YAML
F2_RUN="$("$SFH" run f2-status.yaml --runs-dir f2-status-runs --detach -q 2>/dev/null)"
if [ -n "$F2_RUN" ]; then
  # The heartbeat interval is exactly three seconds. Sleeping exactly three
  # races the heartbeat writer on loaded/macOS runners and can observe the
  # initial null snapshot even though the reader already saw HELLO. Poll with
  # a bounded deadline while the deliberately slow child is still running.
  for _ in 1 2 3 4 5 6 7; do
    if grep -qF '"last_output_utc": "2' "$F2_RUN/status.json"; then
      break
    fi
    sleep 1
  done
  contains "status.json dates the current step" '"step_started_utc": "2' "$F2_RUN/status.json"
  contains "status.json records when a child last spoke" '"last_output_utc": "2' "$F2_RUN/status.json"
  contains "status.json carries the visit number" '"visit": 1' "$F2_RUN/status.json"
  "$SFH" status "$F2_RUN" > f2-status.out 2>/dev/null
  contains "sfh status prints both clocks" "since last output" f2-status.out
  "$SFH" wait "$F2_RUN" > /dev/null 2>&1
else
  echo "FAIL - could not detach the idle-clock status run"
  fail=$((fail + 1))
fi

# --- F4: a resumed run reads a log that carries the new keys ------------------
# Written before the enrichment itself: load_resume takes only the keys it knows
# about, so adding keys must be invisible to it. This cuts a REAL new-format log
# where a crash would cut it - just after a step_end, before its position - and
# resumes from there.
cat > f4res.yaml <<'YAML'
name: f4res
steps:
  - id: one
    cmd: ["echo", "first"]
    route:
      - {when_last_line_is: "first", goto: two}
      - {goto: fail}
  - id: two
    cmd: ["echo", "second:{{steps.one.output | trim}}"]
YAML
"$SFH" run f4res.yaml --runs-dir f4res-runs -q > f4res-live.out 2>&1
check "F4: the flow runs live before being resumed" 0 $?
F4R_DIR="$(dirname "$(find f4res-runs -type f -name 'log.jsonl' -print -quit)")"
awk '{ print } /"event":"step_end"/ && /"step":"one"/ { exit }' \
  "$F4R_DIR/log.jsonl" > "$F4R_DIR/log.cut"
mv "$F4R_DIR/log.cut" "$F4R_DIR/log.jsonl"
"$SFH" run f4res.yaml --resume "$F4R_DIR" -q > f4res.out 2>f4res.err
check "F4: a log carrying the new keys still resumes" 0 $?
contains "F4: the resumed run restored the recorded output" "second:first" f4res.out

# --- F4: position records which rule fired, and on what text -----------------
cat > f4log.yaml <<'YAML'
name: f4log
steps:
  - id: choose
    cmd: ["printf", "prose that mentions VERDICT-OK in passing\nVERDICT-OK\n"]
    route:
      - {when_contains: "no such text anywhere", goto: fail}
      - {when_last_line_is: "VERDICT-OK", goto: whole}
  # TWO lines on purpose. With a one-line step "the head of the routing text"
  # and "the last line" are the same bytes, and the assertion below would hold
  # for an implementation that recorded the last line for every predicate -
  # which is the distinction the key exists to make.
  - id: whole
    cmd: ["sh", "-c", "printf 'whole-text-judgement\nTAIL-LINE\n'"]
    route:
      - {when_contains: "whole-text", goto: catchall}
  - id: catchall
    cmd: ["echo", "no-predicate-here"]
    route:
      - {goto: end}
YAML
"$SFH" run f4log.yaml --runs-dir f4log-runs -q > f4log.out 2>f4log.err
check "F4: the routed flow runs" 0 $?
F4_LOG="$(find f4log-runs -type f -name 'log.jsonl' -print -quit)"
contains "F4: position records the 0-based index of the rule that fired" \
  '"rule":1' "$F4_LOG"
contains "F4: a last-line rule records the last line it judged" \
  '"route_line":"VERDICT-OK"' "$F4_LOG"
contains "F4: a whole-text rule records the head of the routing text" \
  '"route_line":"whole-text-judgement\nTAIL-LINE"' "$F4_LOG"
if grep -F '"via":"catch_all"' "$F4_LOG" | grep -qF '"route_line":"no-predicate-here"'; then
  echo "ok   - F4: the catch-all records the last line too"
  pass=$((pass + 1))
else
  echo "FAIL - F4: the catch-all recorded no route_line"
  grep -F '"event":"position"' "$F4_LOG"
  fail=$((fail + 1))
fi
# A rule index is a claim about `route:`; the vias that never consulted it must
# not make one.
F4_VISITS_LOG="$(find visits-runs -type f -name 'log.jsonl' -print -quit)"
if grep -F '"via":"max_visits"' "$F4_VISITS_LOG" | grep -qF '"rule":'; then
  echo "FAIL - F4: a max_visits position claimed a route rule"
  fail=$((fail + 1))
else
  echo "ok   - F4: only rule/catch_all positions carry a rule index"
  pass=$((pass + 1))
fi

# --- F4: step_end names the OS that produced it -------------------------------
if grep -F '"event":"step_end"' "$F4_LOG" | grep -qE '"os":"(windows|linux|macos)"'; then
  echo "ok   - F4: step_end records the OS it ran on"
  pass=$((pass + 1))
else
  echo "FAIL - F4: step_end has no os field"
  grep -F '"event":"step_end"' "$F4_LOG" | head -1
  fail=$((fail + 1))
fi

# --- F4: step_start records the session it attached to ------------------------
# bin: "echo" stands in for claude exactly as the S2-4 block uses it: sfh
# pre-assigns the session id, so continue_from/fork_from resolve without an AI.
cat > f4sess.yaml <<'YAML'
name: f4sess
steps:
  - id: low
    tool: claude
    bin: "echo"
    access: read
    prompt: "x"
  - id: again
    tool: claude
    bin: "echo"
    access: read
    continue_from: low
    prompt: "y"
YAML
"$SFH" run f4sess.yaml --runs-dir f4sess-runs -q > f4sess.out 2>f4sess.err
F4SESS_LOG="$(find f4sess-runs -type f -name 'log.jsonl' -print -quit)"
if grep -F '"event":"step_start"' "$F4SESS_LOG" | grep -F '"step":"again"' |
  grep -qF '"mode":"continue"'; then
  echo "ok   - F4: step_start records a continue_from parent"
  pass=$((pass + 1))
else
  echo "FAIL - F4: step_start recorded no continue_from parent"
  grep -F '"event":"step_start"' "$F4SESS_LOG"
  fail=$((fail + 1))
fi
if grep -F '"event":"step_start"' "$F4SESS_LOG" | grep -F '"step":"again"' |
  grep -qF '"session_parent":{"id":'; then
  echo "ok   - F4: the recorded parent carries the session it attached to"
  pass=$((pass + 1))
else
  echo "FAIL - F4: the recorded parent has no session id"
  fail=$((fail + 1))
fi
if grep -F '"event":"step_start"' "$F4SESS_LOG" | grep -F '"step":"low"' |
  grep -qF '"session_parent":null'; then
  echo "ok   - F4: a step that opened its own context records a null parent"
  pass=$((pass + 1))
else
  echo "FAIL - F4: a step with no session parent did not record null"
  fail=$((fail + 1))
fi
cat > f4fork.yaml <<'YAML'
name: f4fork
steps:
  - id: low
    tool: claude
    bin: "echo"
    access: read
    prompt: "x"
  - id: branch
    tool: claude
    bin: "echo"
    access: read
    fork_from: low
    prompt: "y"
YAML
"$SFH" run f4fork.yaml --runs-dir f4fork-runs -q > f4fork.out 2>f4fork.err
F4FORK_LOG="$(find f4fork-runs -type f -name 'log.jsonl' -print -quit)"
if grep -F '"event":"step_start"' "$F4FORK_LOG" | grep -F '"step":"branch"' |
  grep -qF '"mode":"fork"'; then
  echo "ok   - F4: a fork_from parent is recorded as a fork"
  pass=$((pass + 1))
else
  echo "FAIL - F4: step_start did not distinguish a fork from a continue"
  grep -F '"event":"step_start"' "$F4FORK_LOG"
  fail=$((fail + 1))
fi

# --- F4: the members of a fan-out record their lineage too ---------------------
# `parallel:` whose children each fork the same warm parent is the documented
# use of fork_from, and the two tests above only cover top-level leaves. Without
# this one, session_parent could be (and was) recorded for every case except the
# one the key was added for.
cat > f4fanp.yaml <<'YAML'
name: f4fanp
steps:
  - id: plan
    tool: claude
    bin: "echo"
    access: read
    prompt: "x"
  - id: fan
    parallel:
      - id: br_a
        tool: claude
        bin: "echo"
        access: read
        fork_from: plan
        prompt: "a"
      - id: br_b
        tool: claude
        bin: "echo"
        access: read
        fork_from: plan
        prompt: "b"
YAML
"$SFH" run f4fanp.yaml --runs-dir f4fanp-runs -q > f4fanp.out 2>f4fanp.err
F4FANP_LOG="$(find f4fanp-runs -type f -name 'log.jsonl' -print -quit)"
for m in br_a br_b; do
  if grep -F '"event":"step_start"' "$F4FANP_LOG" | grep -F "\"step\":\"$m\"" |
    grep -qF '"mode":"fork","step":"plan"'; then
    echo "ok   - F4: fan-out member $m records the session it forked from"
    pass=$((pass + 1))
  else
    echo "FAIL - F4: fan-out member $m recorded no session_parent"
    grep -F '"event":"step_start"' "$F4FANP_LOG"
    fail=$((fail + 1))
  fi
done
if grep -F '"event":"step_start"' "$F4FANP_LOG" | grep -F '"step":"br_a"' |
  grep -qF '"parent":"fan"'; then
  echo "ok   - F4: a member's step_start names the group it belongs to"
  pass=$((pass + 1))
else
  echo "FAIL - F4: a member's step_start does not name its group"
  fail=$((fail + 1))
fi
# ...and that `parent` is load-bearing: it is what keeps load_resume from
# offering a CHILD id as the place to restart. Cut the log at the first member's
# step_start, exactly where a kill mid-fan-out would cut it.
cat > f4fanr.yaml <<'YAML'
name: f4fanr
steps:
  - id: fan
    parallel:
      - {id: m1, cmd: ["echo", "one"]}
      - {id: m2, cmd: ["echo", "two"]}
  - id: after
    cmd: ["echo", "AFTER-RAN"]
YAML
"$SFH" run f4fanr.yaml --runs-dir f4fanr-runs -q > f4fanr1.out 2>f4fanr1.err
check "F4: the fan-out flow runs live before being cut" 0 $?
F4FANR_DIR="$(dirname "$(find f4fanr-runs -type f -name 'log.jsonl' -print -quit)")"
awk '{ print } /"event":"step_start"/ && /"step":"m1"/ { exit }' \
  "$F4FANR_DIR/log.jsonl" > "$F4FANR_DIR/log.cut"
mv "$F4FANR_DIR/log.cut" "$F4FANR_DIR/log.jsonl"
"$SFH" run f4fanr.yaml --resume "$F4FANR_DIR" -q > f4fanr2.out 2>f4fanr2.err
check "F4: a log cut inside a fan-out still resumes" 0 $?
contains "F4: the resume restarts at the GROUP, not at a member" \
  "step 'fan' started" f4fanr2.err
contains "F4: and the flow finishes past the fan-out" "AFTER-RAN" f4fanr2.out

# --- F3: stuck, the third terminal (exit 4) -----------------------------------
# The resume tests come FIRST because resume is where this feature is decided.
# A stuck run is not finished: the work is saved and a human has to look at it,
# so the run stays resumable. A route-borne stuck therefore RE-RUNS the step
# that made the decision instead of replaying its recorded verdict - replaying
# would route to stuck again on the same text, forever.

cat > f3-route-resume.yaml <<'YAML'
name: f3-route-resume
vars:
  verdict: NEEDS-HUMAN
steps:
  - id: judge
    cmd: ["echo", "{{vars.verdict}}"]
    route:
      - when_last_line_is: "OK"
        goto: wrap
      - goto: stuck
  - id: wrap
    cmd: ["echo", "WRAPPED"]
YAML
"$SFH" run f3-route-resume.yaml --runs-dir f3rr-runs -q > f3rr1.out 2> f3rr1.err
check "F3: a route to stuck ends the run with exit 4" 4 $?
F3RR_DIR="$(dirname "$(find f3rr-runs -type f -name 'log.jsonl' -print -quit)")"
"$SFH" run f3-route-resume.yaml --runs-dir f3rr-runs --resume "$F3RR_DIR" --var verdict=OK -q \
  > f3rr2.out 2> f3rr2.err
if ! check "F3: a stuck run is resumable and can go on to succeed" 0 $?; then sed -n '1,20p' f3rr2.err; fi
contains "F3: the resumed run reached the step past the stuck decision" "WRAPPED" f3rr2.out
if [ "$(grep -F '"event":"step_end"' "$F3RR_DIR/log.jsonl" | grep -cF '"step":"judge"')" = "2" ]; then
  echo "ok   - F3: resume re-ran the deciding step instead of replaying its verdict"
  pass=$((pass + 1))
else
  echo "FAIL - F3: the deciding step did not re-run exactly once on resume"
  grep -F '"step":"judge"' "$F3RR_DIR/log.jsonl" | sed -n '1,10p'
  fail=$((fail + 1))
fi

# A stuck reached through on_max_visits is different: resuming re-enters the
# same exhausted node, so it sticks again immediately. That is the honest
# answer - silently resetting the visit counter would be a lie - and the way
# out is to fix max_visits in the flow and use --force-resume.
cat > f3-maxvisits.yaml <<'YAML'
name: f3-maxvisits
steps:
  - id: spin
    max_visits: 2
    on_max_visits: goto:stuck
    cmd: ["echo", "SPIN"]
    route:
      - goto: spin
YAML
"$SFH" run f3-maxvisits.yaml --runs-dir f3mv-runs -q > f3mv1.out 2> f3mv1.err
check "F3: on_max_visits goto:stuck ends the run with exit 4" 4 $?
F3MV_DIR="$(dirname "$(find f3mv-runs -type f -name 'log.jsonl' -print -quit)")"
contains "F3: the max_visits stuck is recorded as such" '"via":"max_visits"' "$F3MV_DIR/log.jsonl"
"$SFH" run f3-maxvisits.yaml --runs-dir f3mv-runs --resume "$F3MV_DIR" -q > f3mv2.out 2> f3mv2.err
check "F3: resuming a max_visits stuck lands on stuck again" 4 $?
if [ "$(grep -F '"event":"step_end"' "$F3MV_DIR/log.jsonl" | grep -cF '"step":"spin"')" = "2" ]; then
  echo "ok   - F3: the resumed max_visits run did not run the exhausted step again"
  pass=$((pass + 1))
else
  echo "FAIL - F3: the resumed max_visits run re-entered the exhausted step"
  fail=$((fail + 1))
fi

cat > f3-basic.yaml <<'YAML'
name: f3-basic
steps:
  - id: work
    cmd: ["echo", "SAVED-WORK"]
  - id: judge
    cmd: ["echo", "NEEDS-HUMAN"]
    route:
      - when_last_line_is: "OK"
        goto: ship
      - goto: stuck
  - id: ship
    cmd: ["echo", "SHIPPED"]
YAML
"$SFH" run f3-basic.yaml --runs-dir f3b-runs -q > f3b.out 2> f3b.err
check "F3: a flow that routes to stuck exits 4" 4 $?
F3B_DIR="$(dirname "$(find f3b-runs -type f -name 'log.jsonl' -print -quit)")"
contains "F3: status.json records the stuck state" '"state": "stuck"' "$F3B_DIR/status.json"
contains "F3: status.json records exit code 4" '"exit_code": 4' "$F3B_DIR/status.json"
contains "F3: the error names the step that routed to stuck" "routed to stuck after 'judge'" "$F3B_DIR/status.json"
contains "F3: the position event records the stuck terminal" '"next":"stuck"' "$F3B_DIR/log.jsonl"
contains "F3: the step after the stuck decision did not run" "NEEDS-HUMAN" f3b.out
if grep -qF "SHIPPED" f3b.out; then
  echo "FAIL - F3: the run continued past the stuck terminal"
  fail=$((fail + 1))
else
  echo "ok   - F3: stuck is terminal, not a jump to the next step"
  pass=$((pass + 1))
fi
"$SFH" status "$F3B_DIR" > f3b-status.out 2> f3b-status.err
check "F3: sfh status exits 4 for a stuck run" 4 $?
contains "F3: sfh status prints the stuck state" "stuck" f3b-status.out
"$SFH" wait "$F3B_DIR" --timeout 5 > f3b-wait.out 2> f3b-wait.err
check "F3: sfh wait exits 4 for a stuck run" 4 $?
"$SFH" runs show "$F3B_DIR" > f3b-show.out 2>&1
contains "F3: runs show reports the run as stuck" "stuck" f3b-show.out

# The partial emit is the failure path's, not the success path's: a stuck run
# has produced real work and the caller needs it.
"$SFH" run f3-basic.yaml --runs-dir f3np-runs --no-partial-emit -q > f3np.out 2> f3np.err
check "F3: --no-partial-emit still exits 4" 4 $?
if [ -s f3np.out ]; then
  echo "FAIL - F3: --no-partial-emit still printed a partial result"
  sed -n '1,10p' f3np.out
  fail=$((fail + 1))
else
  echo "ok   - F3: --no-partial-emit suppresses the stuck partial emit"
  pass=$((pass + 1))
fi

cat > f3-onerror.yaml <<'YAML'
name: f3-onerror
steps:
  - id: attempt
    cmd: ["sh", "-c", "printf 'HALF-DONE\n'; exit 3"]
    on_error: goto:stuck
YAML
"$SFH" run f3-onerror.yaml --runs-dir f3oe-runs -q > f3oe.out 2> f3oe.err
check "F3: on_error goto:stuck ends the run with exit 4" 4 $?
F3OE_DIR="$(dirname "$(find f3oe-runs -type f -name 'log.jsonl' -print -quit)")"
contains "F3: the on_error stuck is recorded as such" '"via":"on_error"' "$F3OE_DIR/log.jsonl"

# `stuck` is a reserved goto target, so it cannot also be a step id - the same
# case-insensitive rule the duplicate-id check uses.
cat > f3-reserved.yaml <<'YAML'
name: f3-reserved
steps:
  - id: stuck
    cmd: ["echo", "x"]
YAML
"$SFH" validate f3-reserved.yaml > f3res.out 2> f3res.err
check "F3: a step id 'stuck' is refused" 2 $?
contains "F3: the refusal says the id is reserved" "reserved" f3res.err
cat > f3-reserved-upper.yaml <<'YAML'
name: f3-reserved-upper
steps:
  - id: STUCK
    cmd: ["echo", "x"]
YAML
"$SFH" validate f3-reserved-upper.yaml > f3resu.out 2> f3resu.err
check "F3: a step id 'STUCK' is refused too (ids compare ignoring case)" 2 $?

# A parallel member's on_error is only ever asked whether it says "continue";
# every goto: spelling was accepted by validate and then ignored, so a member
# that asked for exit 4 got exit 1 - "the plumbing broke, retry" instead of
# "work saved, look at this". Refused where the author can still see it.
for act in "goto:stuck" "goto:end" "goto:fail" "goto:elsewhere"; do
  cat > f3-child-goto.yaml <<YAML
name: f3-child-goto
steps:
  - id: fan
    parallel:
      - {id: ok1, cmd: ["echo", "fine"]}
      - {id: bad, cmd: ["sh", "-c", "exit 5"], on_error: "$act"}
  - id: elsewhere
    cmd: ["echo", "ELSEWHERE"]
YAML
  "$SFH" validate f3-child-goto.yaml > f3cg.out 2> f3cg.err
  check "F3: a parallel member's on_error: $act is refused" 2 $?
  contains "F3: the refusal for $act says goto is not allowed there" \
    "goto is not allowed inside parallel" f3cg.err
done

# A run dir is attacker-writable on a forged report, so a bare `state: stuck`
# gets exactly the authentication `failed` gets - no more trust for being new.
mkdir -p f3-forged
cat > f3-forged/status.json <<'JSON'
{
  "state": "stuck",
  "current_step": "judge",
  "started_utc": "20250101-000000",
  "heartbeat_utc": "20250101-000000",
  "steps_done": 1,
  "cost_usd": 0.0,
  "run_dir": "f3-forged",
  "flow": "x.yaml",
  "pid": 99999,
  "sfh_version": "1.0.0",
  "exit_code": 0,
  "emit_step": null,
  "emit_file": null,
  "error": null,
  "unfinished_step": null,
  "nonce": "forged-nonce"
}
JSON
"$SFH" status f3-forged > f3f-status.out 2> f3f-status.err
check "F3: sfh status does not report a forged stuck as fact" 1 $?
contains "F3: the status refusal names the nonce problem" "nonce" f3f-status.err
"$SFH" wait f3-forged --timeout 5 > f3f-wait.out 2> f3f-wait.err
check "F3: sfh wait does not report a forged stuck as fact" 1 $?
contains "F3: the wait refusal names the nonce problem" "nonce" f3f-wait.err

# --- F6: when_exit routes on the step's own normalized exit code -------------
# The gate an `on_error: continue` probe needs: "it failed, and it failed for
# THE reason I am testing for". Written before the implementation because the
# resume half re-evaluates a recorded route (see the f6res blocks below).
cat > f6exit.yaml <<'YAML'
name: f6exit
steps:
  - id: probe
    cmd: ["sh", "-c", "printf 'HALF-DONE\n'; exit 3"]
    on_error: continue
    route:
      - {when_exit: 0, goto: leaked}
      - {when_exit: 3, goto: expected}
      - {goto: other}
  - id: leaked
    cmd: ["echo", "F6-LEAKED"]
    route:
      - {goto: fail}
  - id: expected
    cmd: ["echo", "F6-EXPECTED"]
    route:
      - {goto: end}
  - id: other
    cmd: ["echo", "F6-OTHER"]
    route:
      - {goto: fail}
YAML
"$SFH" run f6exit.yaml --runs-dir f6exit-runs -q > f6exit.out 2> f6exit.err
check "F6: a step that exits 3 takes the when_exit: 3 branch" 0 $?
contains "F6: the when_exit branch actually ran" "F6-EXPECTED" f6exit.out
F6EXIT_LOG="$(find f6exit-runs -type f -name 'log.jsonl' -print -quit)"
if grep -F '"event":"position"' "$F6EXIT_LOG" | grep -F '"after":"probe"' |
  grep -qF '"via":"rule"'; then
  echo "ok   - F6: a when_exit-only rule logs as a rule, not a catch-all"
  pass=$((pass + 1))
else
  echo "FAIL - F6: a when_exit-only rule was not recorded as a rule"
  grep -F '"event":"position"' "$F6EXIT_LOG"
  fail=$((fail + 1))
fi
contains "F6: the position names the when_exit rule that fired" '"rule":1' "$F6EXIT_LOG"

# The probe idiom from the spec: exit 0 means the attack fixture ran to
# completion, i.e. the guard under test is GONE. A plain "did it fail" check
# passes here; when_exit: 0 is what catches it.
cat > f6probe.yaml <<'YAML'
name: f6probe
steps:
  - id: probe
    cmd: ["sh", "-c", "printf 'attack fixture completed\n'"]
    on_error: continue
    route:
      - {when_exit: 0, goto: broken}
      - {when_stderr_matches: "refusing to resume|no recorded access level", goto: guard_fired}
      - {goto: broken}
  - id: guard_fired
    cmd: ["echo", "F6-GUARD-HELD"]
    route:
      - {goto: end}
  - id: broken
    cmd: ["echo", "F6-GUARD-GONE"]
    route:
      - {goto: fail}
YAML
"$SFH" run f6probe.yaml --runs-dir f6probe-runs -q > f6probe.out 2> f6probe.err
check "F6: a probe that exits 0 is caught as 'the guard is gone'" 1 $?
contains "F6: the probe routed to the broken branch" "F6-GUARD-GONE" f6probe.out

# The same probe, this time refused by the guard: the exit is non-zero AND the
# refusal message is on stderr, so the verification is the one that counts.
cat > f6err.yaml <<'YAML'
name: f6err
steps:
  - id: probe
    cmd: ["sh", "-c", "printf 'sfh: refusing to resume: no recorded access level\n' >&2; exit 3"]
    on_error: continue
    route:
      - {when_exit: 0, goto: broken}
      - {when_exit: 3, when_stderr_matches: "refusing to resume|no recorded access level", goto: guard_fired}
      - {goto: broken}
  - id: guard_fired
    cmd: ["echo", "F6-GUARD-HELD"]
    route:
      - {goto: end}
  - id: broken
    cmd: ["echo", "F6-GUARD-GONE"]
    route:
      - {goto: fail}
YAML
"$SFH" run f6err.yaml --runs-dir f6err-runs -q > f6err.out 2> f6err.err
check "F6: when_exit AND when_stderr_matches accept the right failure" 0 $?
contains "F6: the stderr-verified branch ran" "F6-GUARD-HELD" f6err.out

# Same refusal text, different exit: the AND must refuse it. This is the case
# the "count any non-zero as proof" habit gets wrong.
cat > f6and.yaml <<'YAML'
name: f6and
steps:
  - id: probe
    cmd: ["sh", "-c", "printf 'sfh: refusing to resume: no recorded access level\n' >&2; exit 9"]
    on_error: continue
    route:
      - {when_exit: 3, when_stderr_matches: "refusing to resume", goto: guard_fired}
      - {goto: broken}
  - id: guard_fired
    cmd: ["echo", "F6-GUARD-HELD"]
    route:
      - {goto: end}
  - id: broken
    cmd: ["echo", "F6-WRONG-REASON"]
    route:
      - {goto: fail}
YAML
"$SFH" run f6and.yaml --runs-dir f6and-runs -q > f6and.out 2> f6and.err
check "F6: a matching stderr with the wrong exit does not satisfy the AND" 1 $?
contains "F6: the wrong-reason failure was not counted as proof" "F6-WRONG-REASON" f6and.out

# A group has no exit code of its own, so when_exit sees the composite sfh
# records: 1 when the group hard-failed, 0 otherwise - never a child's 9.
cat > f6group.yaml <<'YAML'
name: f6group
steps:
  - id: fan
    on_error: continue
    parallel:
      - {id: g_ok, cmd: ["echo", "OK"]}
      - {id: g_bad, cmd: ["sh", "-c", "printf BAD; exit 9"]}
    route:
      - {when_exit: 9, goto: raw}
      - {when_exit: 1, goto: dirty}
      - {goto: clean}
  - id: raw
    cmd: ["echo", "F6-GROUP-RAW"]
    route:
      - {goto: fail}
  - id: dirty
    cmd: ["echo", "F6-GROUP-DIRTY"]
    route:
      - {goto: end}
  - id: clean
    cmd: ["echo", "F6-GROUP-CLEAN"]
    route:
      - {goto: fail}
YAML
"$SFH" run f6group.yaml --runs-dir f6group-runs -q > f6group.out 2> f6group.err
check "F6: a fan-out group routes on its composite exit" 0 $?
contains "F6: the group's when_exit saw 1, not the child's 9" "F6-GROUP-DIRTY" f6group.out

# --- F6: resume re-evaluates when_exit / when_stderr_matches the same way ----
# Both predicates read state that step_end already restores (the normalized exit
# and the contained stderr path), so nothing new is persisted and a resume that
# re-runs the recorded route has to pick the same branch.
cat > f6res.yaml <<'YAML'
name: f6res
steps:
  - id: probe
    cmd: ["sh", "-c", "printf 'sfh: refusing to resume: no recorded access level\n' >&2; printf 'PROBE-DONE\n'"]
    route:
      - {when_exit: 0, when_stderr_matches: "refusing to resume", goto: verified}
      - {goto: inconclusive}
  - id: verified
    cmd: ["echo", "F6-VERIFIED"]
    route:
      - {goto: end}
  - id: inconclusive
    cmd: ["echo", "F6-INCONCLUSIVE"]
    route:
      - {goto: end}
YAML
"$SFH" run f6res.yaml --runs-dir f6res-runs -q > f6res-live.out 2> f6res-live.err
check "F6: the stderr-gated flow runs live" 0 $?
contains "F6: the live run took the verified branch" "F6-VERIFIED" f6res-live.out
F6RES_DIR="$(dirname "$(find f6res-runs -type f -name 'log.jsonl' -print -quit)")"
awk '{ print } /"event":"step_end"/ && /"step":"probe"/ { exit }' \
  "$F6RES_DIR/log.jsonl" > "$F6RES_DIR/log.cut"
mv "$F6RES_DIR/log.cut" "$F6RES_DIR/log.jsonl"
"$SFH" run f6res.yaml --resume "$F6RES_DIR" -q > f6res-resume.out 2> f6res-resume.err
check "F6: the resumed run re-evaluates the route" 0 $?
contains "F6: resume picked the same branch as live" "F6-VERIFIED" f6res-resume.out
# The point of the test is the RE-EVALUATION: if the resume had simply re-run
# the probe, the branch would agree for the wrong reason.
F6RES_STARTS="$(grep -cF '"event":"step_start"' "$F6RES_DIR/log.jsonl" | tr -d '[:space:]')"
F6RES_PROBE="$(grep -F '"event":"step_start"' "$F6RES_DIR/log.jsonl" | grep -cF '"step":"probe"' | tr -d '[:space:]')"
check "F6: the resume routed from the record instead of re-running the probe" 1 "$F6RES_PROBE"
[ "$F6RES_STARTS" -ge 2 ] || echo "note: only $F6RES_STARTS step_start events after resume"

# ... and the fail-closed half: the stderr file is what the predicate reads, so
# a run dir whose <id>.err.txt was deleted must NOT match. A deleted artifact is
# missing evidence, never a pass.
"$SFH" run f6res.yaml --runs-dir f6gone-runs -q > f6gone-live.out 2> f6gone-live.err
check "F6: the flow runs live before its stderr file is removed" 0 $?
F6GONE_DIR="$(dirname "$(find f6gone-runs -type f -name 'log.jsonl' -print -quit)")"
awk '{ print } /"event":"step_end"/ && /"step":"probe"/ { exit }' \
  "$F6GONE_DIR/log.jsonl" > "$F6GONE_DIR/log.cut"
mv "$F6GONE_DIR/log.cut" "$F6GONE_DIR/log.jsonl"
rm -f "$F6GONE_DIR/probe.err.txt"
"$SFH" run f6res.yaml --resume "$F6GONE_DIR" -q > f6gone.out 2> f6gone.err
check "F6: a resume whose stderr file is gone still finishes" 0 $?
contains "F6: a missing stderr file makes when_stderr_matches fail closed" \
  "F6-INCONCLUSIVE" f6gone.out
F6GONE_PROBE="$(grep -F '"event":"step_start"' "$F6GONE_DIR/log.jsonl" | grep -cF '"step":"probe"' | tr -d '[:space:]')"
check "F6: the fail-closed resume did not re-run the probe either" 1 "$F6GONE_PROBE"

# A failed probe is also complete when step_end records a typed exit, timeout,
# and interruption state. If sfh stops in the gap before its on_error position,
# resume must replay on_error + route from that record, not execute the external
# probe again (which may mutate state or give a different answer).
rm -f f6-failed-probe-count
cat > f6failedres.yaml <<'YAML'
name: f6failedres
steps:
  - id: probe
    cmd: ["sh", "-c", "printf 'probe\n' >> f6-failed-probe-count; printf 'sfh: refusing to resume: no recorded access level\n' >&2; printf 'PROBE-REFUSED\n'; exit 3"]
    on_error: continue
    route:
      - {when_exit: 3, when_stderr_matches: "refusing to resume", goto: verified}
      - {goto: inconclusive}
  - id: verified
    cmd: ["echo", "F6-FAILED-VERIFIED"]
    route: [{goto: end}]
  - id: inconclusive
    cmd: ["echo", "F6-FAILED-INCONCLUSIVE"]
    route: [{goto: fail}]
YAML
"$SFH" run f6failedres.yaml --runs-dir f6failedres-runs -q > f6failed-live.out 2> f6failed-live.err
check "F6: the failed-probe flow runs live" 0 $?
F6FAILED_DIR="$(dirname "$(find f6failedres-runs -type f -name 'log.jsonl' -print -quit)")"
awk '{ print } /"event":"step_end"/ && /"step":"probe"/ { exit }' \
  "$F6FAILED_DIR/log.jsonl" > "$F6FAILED_DIR/log.cut"
mv "$F6FAILED_DIR/log.cut" "$F6FAILED_DIR/log.jsonl"
"$SFH" run f6failedres.yaml --resume "$F6FAILED_DIR" -q > f6failed-resume.out 2> f6failed-resume.err
check "F6: resume completes from a recorded failed probe" 0 $?
contains "F6: failed-probe resume takes the recorded diagnostic branch" \
  "F6-FAILED-VERIFIED" f6failed-resume.out
F6FAILED_CALLS="$(wc -l < f6-failed-probe-count | tr -d '[:space:]')"
check "F6: failed-probe resume does not execute the probe twice" 1 "$F6FAILED_CALLS"
F6FAILED_STARTS="$(grep -F '"event":"step_start"' "$F6FAILED_DIR/log.jsonl" | grep -cF '"step":"probe"' | tr -d '[:space:]')"
check "F6: failed-probe log contains one probe start across resume" 1 "$F6FAILED_STARTS"

# The same crash gap exists for a failed aggregate_end. Its member snapshot and
# headerless route text are already durable, so on_error: continue can be
# replayed without re-running a failed member.
rm -f f6-failed-group-count
cat > f6failedgroup.yaml <<'YAML'
name: f6failedgroup
steps:
  - id: fan
    on_error: continue
    parallel:
      - id: child
        cmd: ["sh", "-c", "printf 'child\n' >> f6-failed-group-count; printf 'CHILD-FAILED\n'; exit 7"]
    route:
      - {when_exit: 1, goto: expected}
      - {goto: wrong}
  - id: expected
    cmd: ["echo", "F6-FAILED-GROUP-EXPECTED"]
    route: [{goto: end}]
  - id: wrong
    cmd: ["echo", "F6-FAILED-GROUP-WRONG"]
    route: [{goto: fail}]
YAML
"$SFH" run f6failedgroup.yaml --runs-dir f6failedgroup-runs -q \
  > f6failedgroup-live.out 2> f6failedgroup-live.err
check "F6: the failed-group flow runs live" 0 $?
F6FAILEDGROUP_DIR="$(dirname "$(find f6failedgroup-runs -type f -name 'log.jsonl' -print -quit)")"
awk '{ print } /"event":"aggregate_end"/ && /"step":"fan"/ { exit }' \
  "$F6FAILEDGROUP_DIR/log.jsonl" > "$F6FAILEDGROUP_DIR/log.cut"
mv "$F6FAILEDGROUP_DIR/log.cut" "$F6FAILEDGROUP_DIR/log.jsonl"
"$SFH" run f6failedgroup.yaml --resume "$F6FAILEDGROUP_DIR" -q \
  > f6failedgroup-resume.out 2> f6failedgroup-resume.err
check "F6: resume completes from a recorded failed aggregate" 0 $?
contains "F6: failed aggregate resumes through the expected branch" \
  "F6-FAILED-GROUP-EXPECTED" f6failedgroup-resume.out
F6FAILEDGROUP_CALLS="$(wc -l < f6-failed-group-count | tr -d '[:space:]')"
check "F6: failed aggregate resume does not re-run its member" 1 "$F6FAILEDGROUP_CALLS"

# --- F6: validate checks the new regex like the other two --------------------
cat > f6badrx.yaml <<'YAML'
name: f6badrx
steps:
  - id: a
    cmd: ["echo", "x"]
    route:
      - {when_stderr_matches: "([unclosed", goto: end}
YAML
"$SFH" validate f6badrx.yaml > f6badrx.out 2> f6badrx.err
check "F6: a bad when_stderr_matches regex is refused" 2 $?
contains "F6: the refusal names the regex" "bad regex" f6badrx.err

# ...and the template inside it, which is the expensive half. The regex check
# above SKIPS any pattern containing {{ (it cannot compile one until run time),
# so precheck is the only thing standing between a typo here and a run that
# executes - and pays for - the guarded step before dying on the route.
cat > f6badtpl.yaml <<'YAML'
name: f6badtpl
steps:
  - id: probe
    cmd: ["sh", "-c", "echo boom >&2; exit 3"]
    on_error: continue
    route:
      - {when_stderr_matches: "{{vars.nope}}", goto: hit}
      - {goto: miss}
  - id: hit
    cmd: ["echo", "HIT"]
    route: [{goto: end}]
  - id: miss
    cmd: ["echo", "MISS"]
    route: [{goto: end}]
YAML
"$SFH" validate f6badtpl.yaml > f6bt.out 2> f6bt.err
check "F6: an undefined variable in when_stderr_matches is refused by validate" 2 $?
contains "F6: the refusal names the variable" "undefined variable 'nope'" f6bt.err
"$SFH" run f6badtpl.yaml --runs-dir f6bt-runs --dry-run > f6btd.out 2> f6btd.err
check "F6: dry-run refuses it too, before anything is spawned" 2 $?
"$SFH" run f6badtpl.yaml --runs-dir f6bt2-runs -q > f6btr.out 2> f6btr.err
check "F6: and a real run refuses it before the probe step spends anything" 2 $?
if grep -rqF '"event":"step_start"' f6bt2-runs 2>/dev/null; then
  echo "FAIL - F6: the guarded step ran before the bad route template was caught"
  fail=$((fail + 1))
else
  echo "ok   - F6: no step ran before the route template was checked"
  pass=$((pass + 1))
fi

# --- F5: on_budget, the landing before the cliff ------------------------------
# A ceiling is a cliff: max_cost_usd / wall_clock_sec end the run with an error
# and nothing handed back. on_budget turns the last slice of the budget into a
# landing strip - threshold = ceiling - reserve, per axis - so the flow gets one
# chance to wrap up. The resume test comes FIRST, because "once per run" is the
# only part of this feature that a crash can silently undo.

# 1. Once per run, ACROSS a resume. Reported cost survives a resume, so a
#    resumed run arrives with the threshold already crossed and would land a
#    second time if the log did not say it already had. The landing target is
#    `stuck`, the idiom the README recommends: work saved, exit 4, a human
#    looks. Resuming re-runs the step the landing pre-empted.
cat > f5-resume.yaml <<YAML
name: f5-resume
defaults:
  max_cost_usd: 1.0
  on_budget: goto:stuck
  budget_reserve: { cost_usd: 0.95 }
steps:
  - id: spend
    tool: claude
    bin: "$STUB_BIN"
    access: read
    env:
      SFH_STUB_COST: "0.10"
    prompt: "spend some budget"
  - id: after
    cmd: ["echo", "AFTER-RAN"]
YAML
"$SFH" run f5-resume.yaml --runs-dir f5r-runs -q > f5r1.out 2> f5r1.err
check "F5: crossing the cost threshold lands on stuck (exit 4)" 4 $?
F5R_DIR="$(dirname "$(find f5r-runs -type f -name 'log.jsonl' -print -quit)")"
contains "F5: the landing is logged as an event" '"event":"budget_landing"' "$F5R_DIR/log.jsonl"
contains "F5: the landing names the axis that crossed" '"trigger":"cost"' "$F5R_DIR/log.jsonl"
contains "F5: the landing records what had been spent" '"spent_usd":0.1' "$F5R_DIR/log.jsonl"
contains "F5: the landing is recorded as a position, via budget" '"via":"budget"' "$F5R_DIR/log.jsonl"
if grep -qF "AFTER-RAN" f5r1.out; then
  echo "FAIL - F5: the landing did not pre-empt the next step"
  fail=$((fail + 1))
else
  echo "ok   - F5: the step the landing pre-empted did not run"
  pass=$((pass + 1))
fi
"$SFH" run f5-resume.yaml --runs-dir f5r-runs --resume "$F5R_DIR" -q > f5r2.out 2> f5r2.err
check "F5: the resumed run finishes instead of landing again" 0 $?
contains "F5: the resume ran the step the landing had pre-empted" "AFTER-RAN" f5r2.out
F5R_LANDINGS="$(grep -cF '"event":"budget_landing"' "$F5R_DIR/log.jsonl")"
check "F5: one landing per run, resume included" 1 "$F5R_LANDINGS"
"$SFH" runs show "$F5R_DIR" > f5r-show.out 2>&1
contains "F5: runs show reports the landing" "budget  : landed on cost" f5r-show.out

# 2. The wall-clock axis, on its own clock. Threshold is 120 - 118 = 2s, so the
#    first step (3s) puts the run over it while the ceiling itself is still two
#    minutes away - the landing has to be what ends the loop, not the ceiling.
cat > f5-wall.yaml <<YAML
name: f5-wall
defaults:
  wall_clock_sec: 120
  on_budget: goto:wrap
  budget_reserve: { wall_clock_sec: 118 }
steps:
  - id: work
    cmd: ["$STUB_BIN", "--stub-plain", "--stub-sleep", "3", "--stub-last-line", "WORKED"]
    route:
      - goto: work
  - id: wrap
    cmd: ["echo", "WRAPPED"]
YAML
"$SFH" run f5-wall.yaml --runs-dir f5w-runs -q > f5w.out 2> f5w.err
check "F5: a wall-clock landing ends the loop and the run succeeds" 0 $?
F5W_DIR="$(dirname "$(find f5w-runs -type f -name 'log.jsonl' -print -quit)")"
contains "F5: the wall-clock landing names its axis" '"trigger":"wall_clock"' "$F5W_DIR/log.jsonl"
contains "F5: the landing target ran" "WRAPPED" f5w.out
F5W_WORK="$(grep -F '"event":"step_end"' "$F5W_DIR/log.jsonl" | grep -cF '"step":"work"')"
check "F5: the looping step ran once before the landing" 1 "$F5W_WORK"

# 3. The reserve is headroom, not an extension. Once the landing has been spent
#    the ceiling check is all that is left, and a landing chain that eats the
#    reserve too ends the run the old way: an error, fail-closed.
cat > f5-overrun.yaml <<YAML
name: f5-overrun
defaults:
  wall_clock_sec: 5
  on_budget: goto:wrap
  budget_reserve: { wall_clock_sec: 4 }
steps:
  - id: work
    cmd: ["$STUB_BIN", "--stub-plain", "--stub-sleep", "2", "--stub-last-line", "WORKED"]
    route:
      - goto: work
  - id: wrap
    cmd: ["$STUB_BIN", "--stub-plain", "--stub-sleep", "6", "--stub-last-line", "WRAPPED"]
    route:
      - goto: tail
  - id: tail
    cmd: ["echo", "TAIL-RAN"]
YAML
"$SFH" run f5-overrun.yaml --runs-dir f5o-runs -q > f5o.out 2> f5o.err
check "F5: eating the reserve too still fails at the ceiling" 1 $?
contains "F5: the ceiling failure is the old wall_clock_sec one" "exceeded wall_clock_sec" f5o.err
F5O_DIR="$(dirname "$(find f5o-runs -type f -name 'log.jsonl' -print -quit)")"
contains "F5: the landing had fired before the ceiling did" '"event":"budget_landing"' "$F5O_DIR/log.jsonl"
if grep -qF "TAIL-RAN" f5o.out; then
  echo "FAIL - F5: the run continued past the ceiling"
  fail=$((fail + 1))
else
  echo "ok   - F5: the ceiling still stops the run dead"
  pass=$((pass + 1))
fi

# 4. The template variables. remaining_* is the string `unlimited` on an axis
#    with no ceiling - not 0, and not empty, either of which reads as a budget
#    that has run out.
cat > f5-vars.yaml <<'YAML'
name: f5-vars
defaults:
  max_cost_usd: 2.0
steps:
  - id: show
    cmd: ["echo", "SPENT={{budget.spent_usd}} SECS={{budget.elapsed_sec}} RUSD={{budget.remaining_usd}} RSEC={{budget.remaining_sec}}"]
YAML
"$SFH" run f5-vars.yaml --runs-dir f5v-runs -q > f5v.out 2> f5v.err
check "F5: the budget template variables render" 0 $?
contains "F5: spend renders to four decimals" "SPENT=0.0000" f5v.out
contains "F5: elapsed seconds render" "SECS=0" f5v.out
contains "F5: a declared ceiling renders as what is left" "RUSD=2.0000" f5v.out
contains "F5: an undeclared ceiling renders as unlimited" "RSEC=unlimited" f5v.out

# 5. dry-run shows the one goto that appears in no step's route:.
cat > f5-dry.yaml <<'YAML'
name: f5-dry
defaults:
  max_cost_usd: 60.0
  wall_clock_sec: 43200
  on_budget: goto:wrap
  budget_reserve: { cost_usd: 2.0, wall_clock_sec: 900 }
steps:
  - id: work
    cmd: ["echo", "work"]
  - id: wrap
    cmd: ["echo", "wrap"]
YAML
"$SFH" run f5-dry.yaml --runs-dir f5d-runs --dry-run > f5d.out 2> f5d.err
check "F5: a flow with a landing dry-runs" 0 $?
contains "F5: dry-run prints the landing and both reserves" "budget landing: goto wrap (cost reserve \$2.00, wall reserve 900s)" f5d.out

# 6. validate. Each half of the feature is useless without the other, and a
#    landing that names nothing real is a silent dead end - all refused loudly.
cat > f5-bad-reserve.yaml <<'YAML'
name: f5-bad-reserve
defaults:
  max_cost_usd: 10.0
  budget_reserve: { cost_usd: 1.0 }
steps:
  - id: a
    cmd: ["echo", "a"]
YAML
"$SFH" validate f5-bad-reserve.yaml > f5br.out 2> f5br.err
check "F5: a reserve with no on_budget is refused" 2 $?
contains "F5: the refusal names on_budget" "on_budget" f5br.err
cat > f5-bad-noceiling.yaml <<'YAML'
name: f5-bad-noceiling
defaults:
  on_budget: goto:wrap
steps:
  - id: a
    cmd: ["echo", "a"]
  - id: wrap
    cmd: ["echo", "wrap"]
YAML
"$SFH" validate f5-bad-noceiling.yaml > f5bn.out 2> f5bn.err
check "F5: on_budget with no ceiling at all is refused" 2 $?
contains "F5: the refusal names the missing ceilings" "max_cost_usd" f5bn.err
cat > f5-bad-goto.yaml <<'YAML'
name: f5-bad-goto
defaults:
  wall_clock_sec: 60
  on_budget: goto:nowhere
steps:
  - id: a
    cmd: ["echo", "a"]
YAML
"$SFH" validate f5-bad-goto.yaml > f5bg.out 2> f5bg.err
check "F5: a landing on a step that does not exist is refused" 2 $?
contains "F5: the refusal names the missing target" "nowhere" f5bg.err
cat > f5-bad-form.yaml <<'YAML'
name: f5-bad-form
defaults:
  wall_clock_sec: 60
  on_budget: wrap
steps:
  - id: a
    cmd: ["echo", "a"]
  - id: wrap
    cmd: ["echo", "wrap"]
YAML
"$SFH" validate f5-bad-form.yaml > f5bf.out 2> f5bf.err
check "F5: on_budget without the goto: prefix is refused" 2 $?
contains "F5: the refusal shows the accepted spelling" "goto:<id>" f5bf.err
cat > f5-terminals.yaml <<'YAML'
name: f5-terminals
defaults:
  wall_clock_sec: 60
  on_budget: goto:stuck
  budget_reserve: { wall_clock_sec: 30 }
steps:
  - id: a
    cmd: ["echo", "a"]
YAML
"$SFH" validate f5-terminals.yaml > f5t.out 2> f5t.err
check "F5: landing on the stuck terminal validates" 0 $?

# 7. The reserve is not optional. Threshold = ceiling - reserve, so a reserve of
#    zero puts the landing ON the ceiling: the landing fires, logs its event,
#    prints "-> goto wrap", and the ceiling check at the top of the very next
#    iteration ends the run on the same numbers with the landing chain unrun.
#    That is the whole feature doing nothing in its shortest spelling, so it is
#    refused where every other half-configured budget guard is refused. Per axis,
#    because the axes never lend to each other.
cat > f5-no-reserve.yaml <<'YAML'
name: f5-no-reserve
defaults:
  wall_clock_sec: 60
  on_budget: goto:wrap
steps:
  - id: work
    cmd: ["echo", "work"]
  - id: wrap
    cmd: ["echo", "wrap"]
YAML
"$SFH" validate f5-no-reserve.yaml > f5nr.out 2> f5nr.err
check "F5: a landing that holds nothing back is refused" 2 $?
contains "F5: the refusal names the axis with no reserve" "wall_clock_sec" f5nr.err
contains "F5: the refusal says the landing chain would not run" "landing chain unrun" f5nr.err
# The mixed case is the one that hides: cost is reserved, so the flow LOOKS
# configured, and the wall-clock landing is still a no-op.
cat > f5-half-reserve.yaml <<'YAML'
name: f5-half-reserve
defaults:
  max_cost_usd: 10.0
  wall_clock_sec: 60
  on_budget: goto:wrap
  budget_reserve: { cost_usd: 5.0 }
steps:
  - id: work
    cmd: ["echo", "work"]
  - id: wrap
    cmd: ["echo", "wrap"]
YAML
"$SFH" validate f5-half-reserve.yaml > f5hr.out 2> f5hr.err
check "F5: reserving one axis does not buy the other a landing" 2 $?
contains "F5: the refusal names the unreserved axis, not the reserved one" \
  "wall_clock_sec holds nothing back" f5hr.err
# And a run of the shape that used to be silent: it never gets to run at all.
"$SFH" run f5-no-reserve.yaml --runs-dir f5nr-runs -q > f5nrr.out 2> f5nrr.err
check "F5: and the same flow is refused by run, not just by validate" 2 $?
if grep -qF '"event":"budget_landing"' f5nr-runs/*/log.jsonl 2>/dev/null; then
  echo "FAIL - F5: a zero-reserve landing was advertised by a real run"
  fail=$((fail + 1))
else
  echo "ok   - F5: no run ever reports a landing it cannot deliver"
  pass=$((pass + 1))
fi
# --- F1: when_members, the deterministic fan-out vote -------------------------
# Counting agreement by grepping the aggregate is broken three ways: it needs a
# shell, it counts a needle QUOTED inside the prose, and a group's route only
# ever saw the whole concatenation's last line. The decisive fact is subtler: a
# fan-out's ROUTING text carries a failed member's raw output with no banner at
# all (the "[sfh: FAILED]" marker goes on the labeled aggregate only), so no
# amount of text matching can tell "said VOTE-YES" from "said VOTE-YES and
# exited 1". The vote is counted from the engine's own record of each member.
#
# The resume tests come first, per spec invariant 7: this feature decides
# branches from data that has to survive a crash, and a resume that re-decides
# differently from live is the failure mode worth writing down first.

# --- F1 resume: the aggregate_end snapshot decides exactly as live did --------
# vc says the winning words and exits 1. Live must not count it; a resume that
# re-reads the routing text instead of the member record would find three
# VOTE-YES lines and take the other branch.
cat > f1-banner.yaml <<YAML
name: f1-banner
steps:
  - id: fan
    max_parallel: 3
    parallel:
      - id: va
        cmd: ["echo", "VOTE-YES"]
        on_error: continue
      - id: vb
        cmd: ["echo", "VOTE-YES"]
        on_error: continue
      - id: vc
        cmd: ["$STUB_BIN", "--stub-plain", "--stub-last-line", "VOTE-YES", "--stub-exit", "1"]
        on_error: continue
    route:
      - when_members: { last_line_is: "VOTE-YES", at_least: 3 }
        goto: agreed
      - goto: split
  - id: agreed
    cmd: ["echo", "ALL-AGREED"]
    route: [{ goto: end }]
  - id: split
    cmd: ["echo", "NO-CONSENSUS"]
YAML
"$SFH" run f1-banner.yaml --runs-dir f1b-runs -q > f1b.out 2> f1b.err
check "F1: a group whose member failed still routes (on_error: continue)" 0 $?
F1B_DIR="$(dirname "$(find f1b-runs -type f -name 'log.jsonl' -print -quit)")"
contains "F1: the member that said the words but exited 1 is not counted" "NO-CONSENSUS" f1b.out
if grep -qF "ALL-AGREED" f1b.out; then
  echo "FAIL - F1: a failed member was counted as a vote"
  fail=$((fail + 1))
else
  echo "ok   - F1: exit status decides the vote, not the text"
  pass=$((pass + 1))
fi
# The point of the test, stated as a fact about the run dir: the text a string
# count would have read really does hold three copies of the needle.
F1B_LINES="$(grep -c '^VOTE-YES$' "$F1B_DIR/fan.plain.txt")"
check "F1: the routing text carries the failed member's words unmarked" 3 "$F1B_LINES"
contains "F1: aggregate_end snapshots the member that failed" '"exit":1,"id":"vc","last_line":"VOTE-YES","ok":false' "$F1B_DIR/log.jsonl"
contains "F1: aggregate_end snapshots a member that passed" '"exit":0,"id":"va","last_line":"VOTE-YES","ok":true' "$F1B_DIR/log.jsonl"

# Cut the log where a crash between the aggregate and the routing decision would
# have cut it: everything through aggregate_end, nothing after. The snapshot is
# the real writer's, not a hand-built one.
cp -r "$F1B_DIR" f1-snap
awk '/"event":"aggregate_end"/ { print; exit } { print }' "$F1B_DIR/log.jsonl" > f1-snap/log.jsonl
"$SFH" run f1-banner.yaml --resume f1-snap -q > f1snap.out 2> f1snap.err
check "F1: a run killed between the aggregate and the route resumes" 0 $?
contains "F1: the resumed route decides from the snapshot, as live did" "NO-CONSENSUS" f1snap.out
if grep -qF "ALL-AGREED" f1snap.out; then
  echo "FAIL - F1: the resumed route counted the failed member"
  fail=$((fail + 1))
else
  echo "ok   - F1: live and resumed agree on who voted"
  pass=$((pass + 1))
fi

# The same snapshot, same cut, one rule change: two votes are now enough.
# Without this the pair above would also pass an implementation that counts
# nobody at all.
sed 's/at_least: 3/at_least: 2/' f1-banner.yaml > f1-banner2.yaml
"$SFH" run f1-banner2.yaml --runs-dir f1b2-runs -q > f1b2.out 2> f1b2.err
check "F1: the two members that finished cleanly do carry the vote" 0 $?
contains "F1: two clean votes meet at_least: 2" "ALL-AGREED" f1b2.out

# --- F1 resume: a snapshot without member records is refused, never guessed --
# Only reachable by editing a flow onto a run made before the records existed.
# Falling through to the catch-all would change routing by run generation, in
# silence, so it is an error with a name.
# From the ORIGINAL run dir, not from f1-snap: that one has been resumed to
# completion by now, and a completed run is refused for an unrelated reason.
cp -r "$F1B_DIR" f1-nomembers
awk '/"event":"aggregate_end"/ { print; exit } { print }' "$F1B_DIR/log.jsonl" \
  | sed 's/"members":\[[^]]*\],//' > f1-nomembers/log.jsonl
"$SFH" run f1-banner.yaml --resume f1-nomembers -q > f1nm.out 2> f1nm.err
check "F1: a pending route with no member record fails the run" 1 $?
contains "F1: the refusal says the run predates per-member records" "predates per-member route records" f1nm.err
if grep -qE "NO-CONSENSUS|ALL-AGREED" f1nm.out; then
  echo "FAIL - F1: a route with no member record picked a branch anyway"
  fail=$((fail + 1))
else
  echo "ok   - F1: no branch is taken when the votes cannot be known"
  pass=$((pass + 1))
fi

# --- F1 resume: members carried over from a crashed lap still vote ------------
# The other resume shape: the group itself was cut in half, so there is no
# aggregate_end at all. The members that finished are restored and skipped on
# the way back in, and their votes have to come back with them - `all: true`
# fails the moment one of the three goes missing from the count.
cat > f1-carry.yaml <<'YAML'
name: f1-carry
steps:
  - id: fan
    max_parallel: 3
    parallel:
      - id: ca
        cmd: ["echo", "VOTE-YES"]
      - id: cb
        cmd: ["echo", "VOTE-YES"]
      - id: cc
        cmd: ["sh", "-c", "if [ -f f1-carry-trip ]; then echo VOTE-YES; else touch f1-carry-trip; exit 7; fi"]
    route:
      - when_members: { last_line_is: "VOTE-YES", all: true }
        goto: agreed
      - goto: split
  - id: agreed
    cmd: ["echo", "ALL-AGREED"]
    route: [{ goto: end }]
  - id: split
    cmd: ["echo", "NO-CONSENSUS"]
YAML
"$SFH" run f1-carry.yaml --runs-dir f1c-runs -q > f1c1.out 2> f1c1.err
check "F1: the first lap dies on the member that is not primed yet" 1 $?
F1C_DIR="$(ls -d f1c-runs/*/ | head -1 | sed 's:/*$::')"
"$SFH" run f1-carry.yaml --resume "$F1C_DIR" --runs-dir f1c-runs -q > f1c2.out 2> f1c2.err
check "F1: the resumed group runs to the end" 0 $?
contains "F1: members carried over from the dead lap still count as votes" "ALL-AGREED" f1c2.out

# --- F1: the ordinary case, and what the log says about it -------------------
cat > f1-all.yaml <<'YAML'
name: f1-all
steps:
  - id: fan
    max_parallel: 3
    parallel:
      - id: ua
        cmd: ["echo", "VOTE-YES"]
      - id: ub
        cmd: ["echo", "VOTE-YES"]
      - id: uc
        cmd: ["echo", "VOTE-YES"]
    route:
      - when_members: { last_line_is: "VOTE-YES", at_least: 3 }
        goto: agreed
      - goto: split
  - id: agreed
    cmd: ["echo", "ALL-AGREED"]
    route: [{ goto: end }]
  - id: split
    cmd: ["echo", "NO-CONSENSUS"]
YAML
"$SFH" run f1-all.yaml --runs-dir f1a-runs -q > f1a.out 2> f1a.err
check "F1: three members agreeing meet at_least: 3" 0 $?
contains "F1: unanimous agreement takes the agreeing branch" "ALL-AGREED" f1a.out
F1A_DIR="$(dirname "$(find f1a-runs -type f -name 'log.jsonl' -print -quit)")"
contains "F1: the position event records the tally" '"votes":3' "$F1A_DIR/log.jsonl"
contains "F1: the position event names the voters" '"voters":["ua","ub","uc"]' "$F1A_DIR/log.jsonl"
contains "F1: a member rule is logged as a rule, not as the catch-all" '"via":"rule","voters"' "$F1A_DIR/log.jsonl"
contains "F1: the position event names which rule counted" '"rule":0' "$F1A_DIR/log.jsonl"

# --- F1: a needle quoted in the prose is not a vote --------------------------
# The old grep counted any line equal to the needle anywhere in the body. Only
# the last non-empty line of a member's own output decides.
cat > f1-quote.yaml <<YAML
name: f1-quote
steps:
  - id: fan
    max_parallel: 3
    parallel:
      - id: qa
        cmd: ["$STUB_BIN", "--stub-plain", "--stub-quote", "VOTE-YES", "--stub-last-line", "VOTE-NO"]
      - id: qb
        cmd: ["$STUB_BIN", "--stub-plain", "--stub-quote", "VOTE-YES", "--stub-last-line", "VOTE-NO"]
      - id: qc
        cmd: ["$STUB_BIN", "--stub-plain", "--stub-quote", "VOTE-YES", "--stub-last-line", "VOTE-NO"]
    route:
      - when_members: { last_line_is: "VOTE-YES", at_least: 1 }
        goto: agreed
      - goto: split
  - id: agreed
    cmd: ["echo", "ALL-AGREED"]
    route: [{ goto: end }]
  - id: split
    cmd: ["echo", "NO-CONSENSUS"]
YAML
"$SFH" run f1-quote.yaml --runs-dir f1q-runs -q > f1q.out 2> f1q.err
check "F1: a group whose members all quote the needle still routes" 0 $?
F1Q_DIR="$(dirname "$(find f1q-runs -type f -name 'log.jsonl' -print -quit)")"
contains "F1: the needle really is present in the routing text" "VOTE-YES" "$F1Q_DIR/fan.plain.txt"
contains "F1: a needle quoted mid-body is not a vote, even at at_least: 1" "NO-CONSENSUS" f1q.out
contains "F1: the snapshot records the last line, not the quoted one" '"last_line":"VOTE-NO"' "$F1Q_DIR/log.jsonl"

# --- F1: a trailing CR does not cost a member its vote -----------------------
cat > f1-crlf.yaml <<YAML
name: f1-crlf
steps:
  - id: fan
    max_parallel: 3
    parallel:
      - id: ra
        cmd: ["$STUB_BIN", "--stub-plain", "--stub-last-line", 'VOTE-YES\r']
      - id: rb
        cmd: ["$STUB_BIN", "--stub-plain", "--stub-last-line", 'VOTE-YES\r']
      - id: rc
        cmd: ["$STUB_BIN", "--stub-plain", "--stub-last-line", 'VOTE-YES\r']
    route:
      - when_members: { last_line_is: "VOTE-YES", all: true }
        goto: agreed
      - goto: split
  - id: agreed
    cmd: ["echo", "ALL-AGREED"]
    route: [{ goto: end }]
  - id: split
    cmd: ["echo", "NO-CONSENSUS"]
YAML
"$SFH" run f1-crlf.yaml --runs-dir f1r-runs -q > f1r.out 2> f1r.err
check "F1: a CRLF verdict line runs" 0 $?
contains "F1: a verdict line ending in CR still matches" "ALL-AGREED" f1r.out

# --- F1: a verdict line that had to be cut is not a vote ----------------------
# The recorded line stops at 200 characters and the tally compares that. A needle
# of exactly 200 is legal (validate only refuses LONGER ones), so "equal after
# cutting" would count a member that said the verdict and then kept talking -
# a prefix match on the one predicate whose whole job is to be a gate.
NEEDLE_200="$(printf 'A%.0s' $(seq 1 200))"
LONG_250="$(printf 'A%.0s' $(seq 1 250))"
cat > f1-clip.yaml <<YAML
name: f1-clip
steps:
  - id: fan
    max_parallel: 2
    parallel:
      - id: ma
        cmd: ["printf", "%s\\n", "$LONG_250"]
      - id: mb
        cmd: ["printf", "%s\\n", "$NEEDLE_200"]
    route:
      - when_members: { last_line_is: "$NEEDLE_200", at_least: 1 }
        goto: agreed
      - goto: split
  - id: agreed
    cmd: ["echo", "ALL-AGREED"]
    route: [{ goto: end }]
  - id: split
    cmd: ["echo", "NO-CONSENSUS"]
YAML
"$SFH" run f1-clip.yaml --runs-dir f1cl-runs -q > f1cl.out 2> f1cl.err
check "F1: a group with an over-long member line routes" 0 $?
F1CL_DIR="$(dirname "$(find f1cl-runs -type f -name 'log.jsonl' -print -quit)")"
contains "F1: the member whose line fits still votes" "ALL-AGREED" f1cl.out
contains "F1: only the unclipped member is named a voter" '"voters":["mb"]' "$F1CL_DIR/log.jsonl"
contains "F1: the clipped member is recorded as clipped" '"clipped":true,"exit":0,"id":"ma"' "$F1CL_DIR/log.jsonl"
contains "F1: a line that fit whole is recorded as unclipped" '"clipped":false,"exit":0,"id":"mb"' "$F1CL_DIR/log.jsonl"
# The decisive half: unanimity must not be completed by a line nobody can read
# to the end. Both members' RECORDED lines are byte-identical here.
sed 's/at_least: 1/all: true/' f1-clip.yaml > f1-clip-all.yaml
"$SFH" run f1-clip-all.yaml --runs-dir f1cla-runs -q > f1cla.out 2> f1cla.err
check "F1: the same group under all: true still routes" 0 $?
contains "F1: a cut line cannot complete a unanimous vote" "NO-CONSENSUS" f1cla.out
if grep -qF "ALL-AGREED" f1cla.out; then
  echo "FAIL - F1: a prefix match was counted as a vote"
  fail=$((fail + 1))
else
  echo "ok   - F1: an unreadable verdict falls to the non-matching side"
  pass=$((pass + 1))
fi

# --- F1 resume: the denominator comes from the flow, not from the record ------
# A snapshot of N members satisfies `all: true` on its own terms. Editing the
# group to N+1 needs --force-resume, which is exactly when this happens, so the
# resumed run would report unanimity on a member that was never asked - while a
# live run of the same flow takes the other branch.
cat > f1-grow.yaml <<'YAML'
name: f1-grow
steps:
  - id: fan
    max_parallel: 3
    parallel:
      - {id: ga, cmd: ["echo", "VOTE-YES"], on_error: continue}
      - {id: gb, cmd: ["echo", "VOTE-YES"], on_error: continue}
      - {id: gc, cmd: ["echo", "VOTE-YES"], on_error: continue}
    route:
      - when_members: { last_line_is: "VOTE-YES", all: true }
        goto: agreed
      - goto: split
  - id: agreed
    cmd: ["echo", "ALL-AGREED"]
    route: [{ goto: end }]
  - id: split
    cmd: ["echo", "NO-CONSENSUS"]
YAML
"$SFH" run f1-grow.yaml --runs-dir f1g-runs -q > f1g.out 2> f1g.err
check "F1: the three-member group agrees live" 0 $?
contains "F1: three of three is unanimous" "ALL-AGREED" f1g.out
F1G_DIR="$(dirname "$(find f1g-runs -type f -name 'log.jsonl' -print -quit)")"
cp -r "$F1G_DIR" f1-grow-snap
awk '/"event":"aggregate_end"/ { print; exit } { print }' "$F1G_DIR/log.jsonl" \
  > f1-grow-snap/log.jsonl
sed 's/      - {id: gc, .*/&\n      - {id: gd, cmd: ["echo", "VOTE-NO"], on_error: continue}/' \
  f1-grow.yaml > f1-grow4.yaml
"$SFH" run f1-grow4.yaml --runs-dir f1g4-runs -q > f1g4.out 2> f1g4.err
check "F1: the four-member flow runs live" 0 $?
contains "F1: live, the fourth member breaks the unanimity" "NO-CONSENSUS" f1g4.out
"$SFH" run f1-grow4.yaml --resume f1-grow-snap --force-resume -q > f1gr.out 2> f1gr.err
check "F1: resuming a three-vote record under a four-member group fails" 1 $?
contains "F1: the refusal names both counts" \
  "the recorded vote has 3 member(s) but the flow now declares 4" f1gr.err
if grep -qF "ALL-AGREED" f1gr.out; then
  echo "FAIL - F1: a stale snapshot carried a unanimous vote"
  fail=$((fail + 1))
else
  echo "ok   - F1: the resumed vote is counted against the group the flow declares"
  pass=$((pass + 1))
fi

# --- F1: an empty foreach never agrees ---------------------------------------
# `all: true` over nothing is true in logic and wrong here: a fan-out that
# produced no workers has decided nothing (invariant 6, fail-closed).
cat > f1-empty.yaml <<'YAML'
name: f1-empty
steps:
  - id: each
    foreach: { from: "[]", split: json }
    cmd: ["echo", "VOTE-YES"]
    route:
      - when_members: { last_line_is: "VOTE-YES", all: true }
        goto: agreed
      - goto: split
  - id: agreed
    cmd: ["echo", "ALL-AGREED"]
    route: [{ goto: end }]
  - id: split
    cmd: ["echo", "NO-CONSENSUS"]
YAML
"$SFH" run f1-empty.yaml --runs-dir f1e-runs -q > f1e.out 2> f1e.err
check "F1: a foreach that produced no items still routes" 0 $?
contains "F1: all: true over an empty fan-out does not agree" "NO-CONSENSUS" f1e.out
if grep -qF "ALL-AGREED" f1e.out; then
  echo "FAIL - F1: the empty set satisfied all: true"
  fail=$((fail + 1))
else
  echo "ok   - F1: a fan-out with no members has decided nothing"
  pass=$((pass + 1))
fi

# A foreach that DOES produce items counts them, so the test above is measuring
# the empty case and not a foreach that cannot count at all.
sed 's/from: "\[\]"/from: "a\\nb"/; s/split: json/split: lines/' f1-empty.yaml > f1-two.yaml
"$SFH" run f1-two.yaml --runs-dir f1t-runs -q > f1t.out 2> f1t.err
check "F1: a foreach with items runs" 0 $?
contains "F1: every item agreeing satisfies all: true" "ALL-AGREED" f1t.out

# --- F1: what validate refuses ------------------------------------------------
cat > f1-v-leaf.yaml <<'YAML'
name: f1-v-leaf
steps:
  - id: solo
    cmd: ["echo", "VOTE-YES"]
    route:
      - when_members: { last_line_is: "VOTE-YES", at_least: 1 }
        goto: end
YAML
"$SFH" validate f1-v-leaf.yaml > f1vl.out 2> f1vl.err
check "F1: when_members on a step with no members is refused" 2 $?
contains "F1: the refusal says it needs parallel: or foreach:" "parallel" f1vl.err

cat > f1-v-mixed.yaml <<'YAML'
name: f1-v-mixed
steps:
  - id: fan
    parallel:
      - id: m1
        cmd: ["echo", "VOTE-YES"]
    route:
      - when_members: { last_line_is: "VOTE-YES", at_least: 1 }
        when_contains: "something"
        goto: end
YAML
"$SFH" validate f1-v-mixed.yaml > f1vm.out 2> f1vm.err
check "F1: when_members combined with another condition is refused" 2 $?
contains "F1: the refusal says the rule must stand alone" "when_contains" f1vm.err

# F1 x F6: the exclusivity has to cover the predicates F6 added too. On a
# fan-out step when_exit is the GROUP's composite and when_stderr_matches has no
# file to read, so ANDing either with a per-member tally would put two questions
# about two different things under one goto - the same trap the text predicates
# are refused for. Neither branch could write this check alone; it is the merge's.
for pred in 'when_exit: 0' 'when_stderr_matches: "boom"'; do
  cat > f1-v-f6mix.yaml <<YAML
name: f1-v-f6mix
steps:
  - id: fan
    parallel:
      - id: m1
        cmd: ["echo", "VOTE-YES"]
    route:
      - when_members: { last_line_is: "VOTE-YES", at_least: 1 }
        $pred
        goto: end
YAML
  "$SFH" validate f1-v-f6mix.yaml > f1vx.out 2> f1vx.err
  check "F1xF6: when_members combined with ${pred%%:*} is refused" 2 $?
  contains "F1xF6: the refusal names ${pred%%:*}" "${pred%%:*}" f1vx.err
done

cat > f1-v-none.yaml <<'YAML'
name: f1-v-none
steps:
  - id: fan
    parallel:
      - id: m1
        cmd: ["echo", "VOTE-YES"]
    route:
      - when_members: { last_line_is: "VOTE-YES" }
        goto: end
YAML
"$SFH" validate f1-v-none.yaml > f1vn.out 2> f1vn.err
check "F1: when_members with no quantifier is refused" 2 $?
contains "F1: the refusal names both quantifiers" "at_least" f1vn.err

cat > f1-v-both.yaml <<'YAML'
name: f1-v-both
steps:
  - id: fan
    parallel:
      - id: m1
        cmd: ["echo", "VOTE-YES"]
    route:
      - when_members: { last_line_is: "VOTE-YES", at_least: 1, all: true }
        goto: end
YAML
"$SFH" validate f1-v-both.yaml > f1vb.out 2> f1vb.err
check "F1: when_members with two quantifiers is refused" 2 $?

# `all: false` parses (deny_unknown_fields lets the KEY through) and then falls
# to the `_ => false` arm of the tally for ever: a branch the author believes in
# that can never fire. Nothing else in the suite feeds it to validate.
cat > f1-v-allfalse.yaml <<'YAML'
name: f1-v-allfalse
steps:
  - id: fan
    parallel:
      - id: m1
        cmd: ["echo", "VOTE-YES"]
    route:
      - when_members: { last_line_is: "VOTE-YES", all: false }
        goto: end
YAML
"$SFH" validate f1-v-allfalse.yaml > f1va.out 2> f1va.err
check "F1: all: false is refused (it could never match)" 2 $?
contains "F1: the refusal names all: false" "all: false" f1va.err

# A needle longer than the recorded verdict line could not match anything, for
# the same reason a clipped member cannot vote: the comparison happens after the
# cut. 201 characters is one past the edge.
NEEDLE_201="$(printf 'A%.0s' $(seq 1 201))"
cat > f1-v-long.yaml <<YAML
name: f1-v-long
steps:
  - id: fan
    parallel:
      - id: m1
        cmd: ["echo", "VOTE-YES"]
    route:
      - when_members: { last_line_is: "$NEEDLE_201", at_least: 1 }
        goto: end
YAML
"$SFH" validate f1-v-long.yaml > f1vlong.out 2> f1vlong.err
check "F1: a last_line_is longer than the recorded line is refused" 2 $?
contains "F1: the refusal names the cut length" "longer than 200 characters" f1vlong.err

cat > f1-v-zero.yaml <<'YAML'
name: f1-v-zero
steps:
  - id: fan
    parallel:
      - id: m1
        cmd: ["echo", "VOTE-YES"]
    route:
      - when_members: { last_line_is: "VOTE-YES", at_least: 0 }
        goto: end
YAML
"$SFH" validate f1-v-zero.yaml > f1vz.out 2> f1vz.err
check "F1: at_least: 0 is refused (it would match a silent fan-out)" 2 $?

cat > f1-v-over.yaml <<'YAML'
name: f1-v-over
steps:
  - id: fan
    parallel:
      - id: m1
        cmd: ["echo", "VOTE-YES"]
      - id: m2
        cmd: ["echo", "VOTE-YES"]
    route:
      - when_members: { last_line_is: "VOTE-YES", at_least: 3 }
        goto: end
YAML
"$SFH" validate f1-v-over.yaml > f1vo.out 2> f1vo.err
check "F1: asking more votes than a parallel group has members is refused" 2 $?
contains "F1: the refusal counts the members it does have" "2" f1vo.err

# A foreach's size is only known at run time, so the same mistake cannot be
# caught statically - it has to be accepted by validate and never match.
cat > f1-v-fe.yaml <<'YAML'
name: f1-v-fe
steps:
  - id: each
    foreach: { from: "a\nb" }
    cmd: ["echo", "VOTE-YES"]
    route:
      - when_members: { last_line_is: "VOTE-YES", at_least: 99 }
        goto: agreed
      - goto: split
  - id: agreed
    cmd: ["echo", "ALL-AGREED"]
    route: [{ goto: end }]
  - id: split
    cmd: ["echo", "NO-CONSENSUS"]
YAML
"$SFH" validate f1-v-fe.yaml > f1vf.out 2> f1vf.err
check "F1: a foreach quantifier over its run-time size passes validate" 0 $?
"$SFH" run f1-v-fe.yaml --runs-dir f1vf-runs -q > f1vfr.out 2> f1vfr.err
check "F1: and the run it cannot satisfy still finishes" 0 $?
contains "F1: an unsatisfiable foreach quantifier falls to the catch-all" "NO-CONSENSUS" f1vfr.out

echo
echo "engine behaviour: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
