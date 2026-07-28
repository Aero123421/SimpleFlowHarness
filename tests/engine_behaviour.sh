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
"$SFH" run basic.yaml --runs-dir basic-runs -q > basic.out 2> basic.err
check "parallel+foreach+filters run" 0 $?
contains "child output is addressable" "done:AAA" basic.out
BASIC_LOG="$(find basic-runs -type f -name 'log.jsonl' -print -quit)"
contains "a matched route records via=rule" '"via":"rule"' "$BASIC_LOG"
contains "an unmatched route records via=fallthrough" '"via":"fallthrough"' "$BASIC_LOG"
contains "step_end records an output hash" '"output_hash":"fa2bfc19a0708642"' "$BASIC_LOG"
if grep -F '"event":"aggregate_end"' "$BASIC_LOG" |
  grep -F '"step":"fan"' |
  grep -qF '"output_hash":"06b7ba3ef1903648"'; then
  echo "ok   - aggregate hash is computed from the unlabeled plain output"
  pass=$((pass + 1))
else
  echo "FAIL - aggregate hash did not match plain output AAA\\\\n\\\\nBBB"
  fail=$((fail + 1))
fi

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
if [ "$elapsed" -lt 25 ]; then
  echo "ok   - timeout enforced (${elapsed}s)"
  pass=$((pass + 1))
else
  echo "FAIL - timeout not enforced (${elapsed}s)"
  fail=$((fail + 1))
fi

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
  broken: { tool: codex, bin: "false" }
  works:  { tool: codex, bin: "echo" }
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
"$SFH" stop "$RUN_DIR3" > stop.out 2>&1
check "stop reports success" 0 $?
contains "stop says what it killed" "killed pid" stop.out
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

echo
echo "engine behaviour: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
