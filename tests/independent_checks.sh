#!/usr/bin/env bash
# Independent verification of the v1.0.0 hardening.
#
# This is deliberately NOT part of the repo's own test suite. The suite was
# written by the same agent that wrote the fixes, so it can only tell us the
# fixes are self-consistent. These checks were written from the outside, from
# the attack descriptions alone, and they run against a built binary.
#
# Two directions, and both matter:
#   ATTACK  - the thing that was exploitable must still be blocked
#   LEGIT   - the hardening must not have locked out ordinary use
#
# Baselined against v0.9.0 (pre-fix): the ATTACK checks fail there, which is
# the only evidence that they test anything at all.
#
# Usage: bash verify_v1.sh /path/to/sfh[.exe]
set -u

SFH="${1:?usage: verify_v1.sh /path/to/sfh}"
[ -x "$SFH" ] || { echo "not executable: $SFH"; exit 2; }
SFH="$(cd "$(dirname "$SFH")" && pwd)/$(basename "$SFH")"
echo "verifying: $SFH"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK" 2>/dev/null' EXIT
cd "$WORK" || exit 2

pass=0; fail=0; skip=0
ok()   { echo "  PASS  $*"; pass=$((pass+1)); }
bad()  { echo "  FAIL  $*"; fail=$((fail+1)); }
warn() { echo "  SKIP  $*"; skip=$((skip+1)); }
sec()  { echo; echo "=== $* ==="; }

# grep -c exits 1 on zero matches but still prints 0, so read stdout only.
count() { if [ -f "$1" ]; then grep -c "^$2\$" "$1" 2>/dev/null | head -1; else echo 0; fi; }
# ls -d leaves a trailing slash; --resume wants the dir itself.
newest() { ls -d "$1"/*/ 2>/dev/null | head -1 | sed 's:/*$::'; }

# ---------------------------------------------------------------- 1. F-2
# The expensive one. A parallel group that dies partway must not re-run the
# members that already finished - each one is a paid AI call.
sec "1. LEGIT / F-2  resume must not re-run finished parallel members"

mkdir -p c1 && (
cd c1 || exit
cat > flow.yaml <<'YAML'
name: f2-resume
steps:
  - id: fan
    max_parallel: 3
    parallel:
      - id: m1
        cmd: ["sh", "-c", "echo m1 >> ../counter.txt; echo out1"]
      - id: m2
        cmd: ["sh", "-c", "echo m2 >> ../counter.txt; echo out2"]
      - id: m3
        cmd: ["sh", "-c", "echo m3 >> ../counter.txt; if [ -f ../tripwire ]; then echo out3; else touch ../tripwire; exit 7; fi"]
  - id: after
    cmd: ["echo", "finished"]
YAML
# First pass: m1 and m2 succeed, m3 fails once and arms the tripwire.
"$SFH" run flow.yaml --runs-dir runs -q >/dev/null 2>&1
rd="$(ls -d runs/*/ 2>/dev/null | head -1 | sed 's:/*$::')"
if [ -z "$rd" ]; then
  echo "NORUN 0 0" > ../c1.verdict
else
  # Second pass: same run resumed. m3 now succeeds. m1/m2 must stay untouched.
  "$SFH" run flow.yaml --resume "$rd" --runs-dir runs -q >/dev/null 2>&1
  printf '%s %s %s\n' \
    "$(if [ -f ../counter.txt ]; then grep -c '^m1$' ../counter.txt 2>/dev/null | head -1; else echo 0; fi)" \
    "$(if [ -f ../counter.txt ]; then grep -c '^m2$' ../counter.txt 2>/dev/null | head -1; else echo 0; fi)" \
    "$(if [ -f ../counter.txt ]; then grep -c '^m3$' ../counter.txt 2>/dev/null | head -1; else echo 0; fi)" \
    > ../c1.verdict
fi
)
n1=""; n2=""; n3=""
[ -f c1.verdict ] && read -r n1 n2 n3 < c1.verdict
if [ "${n1:-}" = "NORUN" ] || [ -z "${n1:-}" ]; then
  bad "F-2: the first pass produced no run dir at all"
elif [ "$n1" = "1" ] && [ "$n2" = "1" ] && [ "$n3" = "2" ]; then
  ok "F-2: m1 and m2 ran once, m3 ran twice - finished members were reused"
elif [ "$n1" != "1" ] || [ "$n2" != "1" ]; then
  bad "F-2: DOUBLE BILLING - finished members re-ran (m1=$n1 m2=$n2 m3=$n3, want 1 1 2)"
else
  bad "F-2: unexpected counts m1=$n1 m2=$n2 m3=$n3 (want 1 1 2)"
fi

# ---------------------------------------------------------------- 2. F-6
# A run dir is untrusted input. Anyone who can write one must not be able to
# make `wait` or `status` report success.
sec "2. ATTACK / F-6  forged status.json must not report success"

forge() { mkdir -p "$1"; printf '%s' "$2" > "$1/status.json"; }

# An exit code is NOT enough here, and assuming it was hid a real leak: `wait`
# on a forged state:"failed" returns non-zero because the run "failed", while
# still running print_result and emitting the file the forged status named. A
# run dir an attacker can write was a file-read primitive, and the non-zero
# exit made it look like the refusal had worked. Every one of these therefore
# checks the OUTPUT as well.
leak_probe() { # leak_probe <dir> <state> <exit_code>
  mkdir -p "$1"
  printf 'TOP-SECRET-%s\n' "$2" > "$1/leak.txt"
  printf '{"state":"%s","exit_code":%s,"pid":999999,"heartbeat":"20260728-000000","steps":1,"emit_step":"x","emit_file":"leak.txt","error":"boom","run_dir":"%s","flow":"x.yaml","sfh_version":"1.0.0","nonce":"deadbeef"}' \
    "$2" "$3" "$1" > "$1/status.json"
}

for st in failed stopped dead done; do
  leak_probe "f_$st" "$st" 0
  out="$("$SFH" wait "f_$st" --timeout 5 2>/dev/null)"
  rc=$?
  case "$out" in
    *TOP-SECRET*) bad "wait: LEAKED the emit file of a forged '$st' run dir" ;;
    *)            ok  "wait: emitted nothing for a forged '$st' run dir" ;;
  esac
  if [ "$rc" -ne 0 ]
    then ok  "wait: forged '$st' with exit_code:0 is not success"
    else bad "wait: FAIL-OPEN - forged '$st' with exit_code:0 returned 0"; fi
  out="$("$SFH" status "f_$st" 2>/dev/null)"
  case "$out" in
    *TOP-SECRET*) bad "status: LEAKED the emit file of a forged '$st' run dir" ;;
    *)            ok  "status: emitted nothing for a forged '$st' run dir" ;;
  esac
done

forge f_done '{"state":"done","exit_code":0,"pid":999999,"heartbeat":"20260728-000000","steps":1}'
: > f_done/log.jsonl                       # present but empty - the old-format loophole
if "$SFH" status f_done >/dev/null 2>&1
  then bad "status: FAIL-OPEN - forged done with no nonce and empty log returned 0"
  else ok  "status: forged done with no nonce and empty log is not success"; fi
if "$SFH" wait f_done --timeout 5 >/dev/null 2>&1
  then bad "wait: FAIL-OPEN - forged done with no nonce and empty log returned 0"
  else ok  "wait: forged done with no nonce and empty log is not success"; fi

# The check must happen BEFORE anything is printed, or the leak already happened.
out="$("$SFH" status f_done 2>/dev/null)"
case "$out" in
  *done*) bad "status: printed a 'done' result on stdout before rejecting it" ;;
  *)      ok  "status: printed no result for the forged run" ;;
esac

# ---------------------------------------------------------------- 3. stop
# `stop` reads a pid out of an untrusted run dir and kills it. It must prove
# the pid really is an sfh run first.
sec "3. ATTACK / stop must not kill an unrelated live process"

sleep 300 &
victim=$!
sleep 1
if kill -0 "$victim" 2>/dev/null; then
  forge f_stop "{\"state\":\"running\",\"pid\":$victim,\"heartbeat\":\"$(date -u +%Y%m%d-%H%M%S)\",\"steps\":1}"
  : > f_stop/log.jsonl
  "$SFH" stop f_stop >/dev/null 2>&1
  sleep 1
  if kill -0 "$victim" 2>/dev/null
    then ok  "stop: refused to kill a pid that is not an sfh run"
    else bad "stop: KILLED AN UNRELATED PROCESS (pid $victim)"; fi
  kill "$victim" 2>/dev/null
else
  warn "stop: could not start a victim process to test with"
fi

# ---------------------------------------------------------------- 4. name
# The flow name becomes a path segment: format!("{}-{}", utc_stamp(), name).
# The timestamp prefix means reserved device names like `con` are harmless;
# separators and traversal are not.
sec "4. ATTACK / flow name must not escape the runs directory"

for n in '../../pwned' 'a/b' 'a\b' '..' '.' 'x*y'; do
  printf 'name: "%s"\nsteps:\n  - id: a\n    cmd: ["echo", "hi"]\n' "$n" > bad_name.yaml
  if "$SFH" validate bad_name.yaml >/dev/null 2>&1
    then bad "validate accepted the flow name '$n'"
    else ok  "validate rejected the flow name '$n'"; fi
done

# ---------------------------------------------------------------- 5. legit names
sec "5. LEGIT / ordinary flow names must still work"

for n in '日本語のフロー' 'my flow v1.2' 'review-pr-42' 'flow.with.dots'; do
  printf 'name: "%s"\nsteps:\n  - id: a\n    cmd: ["echo", "hi"]\n' "$n" > good_name.yaml
  if "$SFH" validate good_name.yaml >/dev/null 2>&1
    then ok  "validate accepted '$n'"
    else bad "validate REJECTED the legitimate name '$n'"; fi
done

# A Japanese name must also survive an actual run - the run dir is made from it.
printf 'name: "日本語のフロー"\nsteps:\n  - id: a\n    cmd: ["echo", "hi"]\n' > jp.yaml
if "$SFH" run jp.yaml --runs-dir jpruns -q >/dev/null 2>&1
  then ok  "run: a Japanese flow name produced a working run dir"
  else bad "run: a Japanese flow name FAILED at run time (validate passed)"; fi

# ---------------------------------------------------------------- 6. F-5
# The bin:/cwd: template check must fire at validate time, not after the
# upstream step has already been paid for.
sec "6. F-5  bin:/cwd: template checks must fire before anything is spent"

cat > tpl_bin.yaml <<'YAML'
name: tpl-bin
steps:
  - id: a
    cmd: ["echo", "/bin/sh"]
  - id: b
    tool: codex
    bin: "{{steps.a.output}}"
    prompt: "hi"
YAML
if "$SFH" validate tpl_bin.yaml >/dev/null 2>&1
  then bad "validate accepted a step-derived bin: (run time rejects it - wasted spend)"
  else ok  "validate rejected a step-derived bin: before any step ran"; fi

cat > tpl_cwd.yaml <<'YAML'
name: tpl-cwd
steps:
  - id: a
    cmd: ["echo", "/tmp"]
  - id: b
    cmd: ["echo", "hi"]
    cwd: "{{steps.a.output}}"
YAML
if "$SFH" validate tpl_cwd.yaml >/dev/null 2>&1
  then bad "validate accepted a step-derived cwd:"
  else ok  "validate rejected a step-derived cwd: before any step ran"; fi

# The guard must not reject a literal bin:/cwd: with no template in it.
cat > tpl_ok.yaml <<'YAML'
name: tpl-ok
steps:
  - id: b
    cmd: ["echo", "hi"]
    cwd: "."
YAML
if "$SFH" validate tpl_ok.yaml >/dev/null 2>&1
  then ok  "LEGIT: a literal cwd: is still allowed"
  else bad "LEGIT: the guard now rejects a literal cwd: too"; fi

# ---------------------------------------------------------------- 7. F-9
# A shell wrapped in argv form is still a shell.
sec "7. ATTACK / F-9  argv-wrapped shells must get the string-form checks"

# Each shell with the flag it actually takes. cmd.exe uses /c, not -c, and
# PowerShell accepts any unambiguous prefix of -Command. A path-qualified or
# .exe-suffixed name must resolve the same way - and identically on all three
# OSes, so the Windows path is listed even though this may run on Linux.
while read -r sh0 flag; do
  [ -n "$sh0" ] || continue
  cat > argv_shell.yaml <<YAML
name: argv-shell
steps:
  - id: a
    cmd: ["echo", "x"]
  - id: b
    cmd: ["$sh0", "$flag", "echo {{steps.a.output}}"]
YAML
  if "$SFH" validate argv_shell.yaml >/dev/null 2>&1
    then bad "validate accepted an untrusted expansion inside ['$sh0','$flag',...]"
    else ok  "validate treated ['$sh0','$flag',...] as a shell"; fi
done <<'SHELLS'
sh -c
/bin/sh -c
bash -c
bash -lc
zsh -c
dash -c
ksh -c
cmd /c
cmd.exe /c
C:\Windows\System32\cmd.exe /c
cmd /k
powershell -Command
powershell -c
powershell -Comm
pwsh -Command
pwsh -EncodedCommand
pwsh /Command
SHELLS

# The same check must not break the argv form that is NOT a shell.
cat > argv_plain.yaml <<'YAML'
name: argv-plain
steps:
  - id: a
    cmd: ["echo", "x"]
  - id: b
    cmd: ["echo", "{{steps.a.output}}"]
YAML
if "$SFH" validate argv_plain.yaml >/dev/null 2>&1
  then ok  "LEGIT: ['echo', '{{steps.a.output}}'] still allowed (not a shell)"
  else bad "LEGIT: the shell check now rejects plain argv form too"; fi

# The other direction, and the one that matters most: `sh -c SCRIPT name arg`
# passes arg as $1 INSIDE the script, where the shell never re-parses it. That
# is the recommended way to give a shell an untrusted value, so refusing it
# would push people toward interpolating into the script text instead - the
# opposite of what the guard is for.
cat > argv_positional.yaml <<'YAML'
name: argv-positional
steps:
  - id: a
    cmd: ["echo", "x"]
  - id: b
    cmd: ["sh", "-c", "cat \"$1\"", "argv-positional", "{{steps.a.output}}"]
YAML
if "$SFH" validate argv_positional.yaml >/dev/null 2>&1
  then ok  "LEGIT: a template in a POSITIONAL arg of sh -c is allowed (it is \$1, not script text)"
  else bad "LEGIT: sh -c positional args are refused - the safest pattern is locked out"; fi

# But cmd.exe and PowerShell really do re-join the tail into one command line,
# so there the trailing argument IS script text and must still be refused.
cat > argv_cmd_tail.yaml <<'YAML'
name: argv-cmd-tail
steps:
  - id: a
    cmd: ["echo", "x"]
  - id: b
    cmd: ["cmd", "/c", "echo", "{{steps.a.output}}"]
YAML
if "$SFH" validate argv_cmd_tail.yaml >/dev/null 2>&1
  then bad "cmd /c joins its tail into one command line - the template is script text"
  else ok  "validate treated the tail of cmd /c as shell text"; fi

cat > argv_ps_tail.yaml <<'YAML'
name: argv-ps-tail
steps:
  - id: a
    cmd: ["echo", "x"]
  - id: b
    cmd: ["pwsh", "-Command", "echo", "{{steps.a.output}}"]
YAML
if "$SFH" validate argv_ps_tail.yaml >/dev/null 2>&1
  then bad "pwsh -Command joins its tail too - the template is script text"
  else ok  "validate treated the tail of pwsh -Command as shell text"; fi

# ---------------------------------------------------------------- 8. string cmd
sec "8. string-form cmd: templates rejected, plain strings still fine"

printf 'name: str-tpl\nsteps:\n  - id: a\n    cmd: ["echo", "x"]\n  - id: b\n    cmd: "echo {{steps.a.output}}"\n' > str_tpl.yaml
if "$SFH" validate str_tpl.yaml >/dev/null 2>&1
  then bad "validate accepted an untrusted expansion in a string cmd:"
  else ok  "validate rejected an untrusted expansion in a string cmd:"; fi

printf 'name: str-plain\nsteps:\n  - id: b\n    cmd: "echo hello"\n' > str_plain.yaml
if "$SFH" validate str_plain.yaml >/dev/null 2>&1
  then ok  "LEGIT: a plain string cmd: with no template still validates"
  else bad "LEGIT: string cmd: is now rejected even without a template"; fi

# ---------------------------------------------------------------- 9. F-10
# A step_end line carrying only exit:0 must not let a resume skip the step.
sec "9. ATTACK / F-10  a step_end with only exit:0 must not count as success"

# The property is "not restored as a SUCCESS", so read what a LATER step
# actually receives: a restored success hands on the clean text, a restored
# non-success hands on sfh's "did not complete" marker instead. Testing the
# downstream value is what makes this a real check - the step being skipped or
# re-run is an implementation choice, but silently passing a forged success
# downstream is the defect.
cat > f10.yaml <<'YAML'
name: f10-resume
steps:
  - id: one
    cmd: ["echo", "CLEANTEXT"]
  - id: two
    cmd: ["sh", "-c", "if [ -f t10r ]; then echo ok; else touch t10r; exit 5; fi"]
  - id: three
    cmd: ["echo", "{{steps.one.output}}"]
YAML
"$SFH" run f10.yaml --runs-dir r10f -q >/dev/null 2>&1
rd10f="$(newest r10f)"
if [ -z "$rd10f" ] || [ ! -f "$rd10f/log.jsonl" ]; then
  bad "F-10: could not set up the run"
else
  # Sanity: the fields this attack deletes must actually be there to delete.
  if ! grep -q '"timed_out":false' "$rd10f/log.jsonl"; then
    bad "F-10: the log has no timed_out field - the tamper would be a no-op"
  fi
  sed -e 's/,"timed_out":false//g' -e 's/,"interrupted":false//g' \
      -e 's/"timed_out":false,//g' -e 's/"interrupted":false,//g' \
      "$rd10f/log.jsonl" > "$rd10f/l.new" && mv "$rd10f/l.new" "$rd10f/log.jsonl"
  out10="$("$SFH" run f10.yaml --resume "$rd10f" --runs-dir r10f -q 2>/dev/null)"
  case "$out10" in
    *"did not complete"*)
      ok "F-10: a step_end missing timed_out/interrupted was not restored as a success" ;;
    *CLEANTEXT*)
      bad "F-10: FAIL-OPEN - the forged success was handed downstream as a clean result" ;;
    *)
      ok "F-10: the tampered record produced no restored output at all" ;;
  esac
fi

# ------------------------------------------------------- 10-12. F-1/F-3/F-7
# All three live in one place: how much the recorded session access is
# trusted. They pull in different directions, which is exactly why they are
# worth checking together.
#   F-3  key absent  (an old run, written before access was recorded) -> allow
#   F-1  key present but not a valid level                            -> refuse
#   F-7  key present and valid but FORGED                             -> refuse
# A fix that satisfies any one of these by collapsing the distinction breaks
# another. `bin: "echo"` stands in for the CLI so no AI is called.
#
# Shape: low(read) -> gate(fails once, so the run stops with low's session
# already recorded) -> cont(continue_from low). Tamper, then resume.
sess_flow() { # sess_flow <file> <trip> <low-access> <cont-access>
  cat > "$1" <<YAML
name: $(basename "$1" .yaml)
steps:
  - id: low
    tool: claude
    bin: "echo"
    access: $3
    prompt: "x"
  - id: gate
    cmd: ["sh", "-c", "if [ -f $2 ]; then echo ok; else touch $2; exit 5; fi"]
  - id: cont
    tool: claude
    bin: "echo"
    access: $4
    continue_from: low
    prompt: "y"
YAML
}
# An sfh 0.x run: no access key in the session, and meta.json says 0.x. Both
# halves matter - the version is what tells a genuinely old run apart from a
# 1.x log someone deleted the field out of.
make_legacy() { # make_legacy <run-dir> <also-drop-version?>
  sed -e 's/,"access":"[a-z]*"//g' -e 's/"access":"[a-z]*",//g' \
      "$1/log.jsonl" > "$1/l.new" && mv "$1/l.new" "$1/log.jsonl"
  if [ "${2:-yes}" = "yes" ]; then
    sed -e 's/"sfh_version": *"[^"]*"/"sfh_version":"0.9.0"/' \
        "$1/meta.json" > "$1/m.new" && mv "$1/m.new" "$1/meta.json"
  fi
}

# Judge these by the REFUSAL MESSAGE, not the exit code. `bin: "echo"` reports
# no session id, so every one of these resumes fails the separate F-11 "resume
# unverified" check as well - and an exit-code test would happily call that a
# blocked escalation. What is being measured is whether the ACCESS guard fired.
blocked_by_access() { grep -qE "refusing to resume|no recorded access level" "$1"; }

sec "10. LEGIT / F-3  an sfh 0.x run must still resume its session at full"
sess_flow s10.yaml t10 full full
"$SFH" run s10.yaml --runs-dir r10 -q >/dev/null 2>&1
rd10="$(newest r10)"
if [ -z "$rd10" ] || [ ! -f "$rd10/log.jsonl" ]; then
  bad "F-3: could not set up the run"
elif ! grep -q '"access":"full"' "$rd10/log.jsonl"; then
  bad "F-3: no session access was recorded at all - check the stub still works"
else
  make_legacy "$rd10"
  "$SFH" run s10.yaml --resume "$rd10" --runs-dir r10 -q >/dev/null 2>e10.txt
  if blocked_by_access e10.txt
    then bad "F-3: LOCKED OUT - the access guard blocked a 0.x run resuming its own full session"
    else ok  "F-3: the access guard let a 0.x run resume at full"; fi
fi

sec "11. ATTACK / F-1  a deleted or invalid access must not become a bypass"

# (a) the access key deleted from a 1.x run - that is tampering, not age.
sess_flow s11a.yaml t11a full full
"$SFH" run s11a.yaml --runs-dir r11a -q >/dev/null 2>&1
rd11a="$(newest r11a)"
if [ -z "$rd11a" ]; then bad "F-1(a): could not set up the run"; else
  make_legacy "$rd11a" no          # drop the key, keep the 1.x version
  "$SFH" run s11a.yaml --resume "$rd11a" --runs-dir r11a -q >/dev/null 2>e11a.txt
  if blocked_by_access e11a.txt
    then ok  "F-1(a): a 1.x log with access deleted was refused by the access guard"
    else bad "F-1(a): FAIL-OPEN - deleting access from a 1.x log passed the access guard"; fi
fi

# (b) a garbage level, resuming at read - the lowest tier must not be a free pass.
sess_flow s11b.yaml t11b read read
"$SFH" run s11b.yaml --runs-dir r11b -q >/dev/null 2>&1
rd11b="$(newest r11b)"
if [ -z "$rd11b" ]; then bad "F-1(b): could not set up the run"; else
  sed -e 's/"access":"read"/"access":"bogus"/g' \
      "$rd11b/log.jsonl" > "$rd11b/l.new" && mv "$rd11b/l.new" "$rd11b/log.jsonl"
  "$SFH" run s11b.yaml --resume "$rd11b" --runs-dir r11b -q >/dev/null 2>e11b.txt
  if blocked_by_access e11b.txt
    then ok  "F-1(b): a garbage access level was refused even at read"
    else bad "F-1(b): FAIL-OPEN - a garbage access level passed the access guard at read"; fi
fi

# (c) the 0.x path must not itself be an escalation route: a legacy run whose
# step really was `read` must still refuse a `full` continuation.
sess_flow s11c.yaml t11c read full
"$SFH" run s11c.yaml --runs-dir r11c -q >/dev/null 2>&1
rd11c="$(newest r11c)"
if [ -z "$rd11c" ]; then bad "F-1(c): could not set up the run"; else
  make_legacy "$rd11c"
  "$SFH" run s11c.yaml --resume "$rd11c" --runs-dir r11c -q >/dev/null 2>e11c.txt
  if blocked_by_access e11c.txt
    then ok  "F-1(c): the 0.x path still refuses a genuine read -> full escalation"
    else bad "F-1(c): FAIL-OPEN - claiming to be a 0.x run passed read -> full"; fi
fi

sec "12. ATTACK / F-7  a forged access:full must not authorise a full resume"
sess_flow s12.yaml t12 read full
"$SFH" run s12.yaml --runs-dir r12 -q >/dev/null 2>&1
rd12="$(newest r12)"
if [ -z "$rd12" ] || [ ! -f "$rd12/log.jsonl" ]; then
  bad "F-7: could not set up the run"
else
  # Sanity: untampered, the escalation read -> full must already be refused.
  "$SFH" run s12.yaml --resume "$rd12" --runs-dir r12 -q >/dev/null 2>e12a.txt
  if blocked_by_access e12a.txt
    then ok  "F-7: baseline - read -> full is refused when the log is honest"
    else bad "F-7: baseline broken - read -> full passed the access guard untampered"; fi
  sed -e 's/"access":"read"/"access":"full"/g' \
      "$rd12/log.jsonl" > "$rd12/l.new" && mv "$rd12/l.new" "$rd12/log.jsonl"
  "$SFH" run s12.yaml --resume "$rd12" --runs-dir r12 -q >/dev/null 2>e12b.txt
  if blocked_by_access e12b.txt
    then ok  "F-7: the forged access:full in the run log was not trusted"
    else bad "F-7: FAIL-OPEN - editing the run log to access:full passed the access guard"; fi
fi

# ---------------------------------------------------------------- 13. F-8
# The privileged-field guard rejects keys beginning with `steps.`. But notes,
# foreach items and argv[0] are equally run-derived, and argv[0] chooses which
# program runs. Only the statically checkable half is here; the vars-restored-
# from-meta.json half needs a resume and is left to the reviewers.
sec "13. ATTACK / F-8  every run-derived value must be barred from bin:/cwd:/argv[0]"

# access: read is REQUIRED here. Without it an AI step is rejected for having
# no access at all, and this check would pass without the privileged-template
# guard existing - a reviewer caught exactly that in an earlier version.
printf 'name: f8-notes\nsteps:\n  - id: b\n    tool: codex\n    access: read\n    bin: "{{notes}}"\n    prompt: "hi"\n' > f8a.yaml
if "$SFH" validate f8a.yaml >/dev/null 2>&1
  then bad "validate accepted bin: {{notes}} - notes are run-derived"
  else ok  "validate rejected bin: {{notes}}"; fi
# Control: the same step with a literal bin must validate, so the check above
# is known to be failing on the template and not on the step's shape.
printf 'name: f8-notes-ok\nsteps:\n  - id: b\n    tool: codex\n    access: read\n    bin: "codex"\n    prompt: "hi"\n' > f8a_ok.yaml
if "$SFH" validate f8a_ok.yaml >/dev/null 2>&1
  then ok  "control: the same step with a literal bin validates"
  else bad "control: the step shape itself is invalid - the {{notes}} check proves nothing"; fi

# foreach takes {from: ...}; a bare list is a syntax error, which would make
# this check pass for the wrong reason too.
printf 'name: f8-item\nsteps:\n  - id: e\n    foreach: { from: "a\\nb" }\n    cmd: ["echo", "x"]\n    cwd: "{{item}}"\n' > f8b.yaml
if "$SFH" validate f8b.yaml >/dev/null 2>&1
  then bad "validate accepted cwd: {{item}} - foreach items are run-derived"
  else ok  "validate rejected cwd: {{item}}"; fi
printf 'name: f8-item-ok\nsteps:\n  - id: e\n    foreach: { from: "a\\nb" }\n    cmd: ["echo", "{{item}}"]\n' > f8b_ok.yaml
if "$SFH" validate f8b_ok.yaml >/dev/null 2>&1
  then ok  "control: the same foreach with {{item}} in a data slot validates"
  else bad "control: the foreach shape itself is invalid - the cwd check proves nothing"; fi

printf 'name: f8-argv0\nsteps:\n  - id: a\n    cmd: ["echo", "x"]\n  - id: b\n    cmd: ["{{steps.a.output}}", "hi"]\n' > f8c.yaml
if "$SFH" validate f8c.yaml >/dev/null 2>&1
  then bad "validate accepted a step-derived argv[0] - that picks which program runs"
  else ok  "validate rejected a step-derived argv[0]"; fi

# And the guard must not reject a template in a non-privileged argv slot.
printf 'name: f8-ok\nsteps:\n  - id: a\n    cmd: ["echo", "x"]\n  - id: b\n    cmd: ["echo", "{{steps.a.output}}"]\n' > f8d.yaml
if "$SFH" validate f8d.yaml >/dev/null 2>&1
  then ok  "LEGIT: a template in a later argv slot is still allowed"
  else bad "LEGIT: the argv[0] guard now rejects templates in every slot"; fi

# ------------------------------------------------------------- 14. regressions
# Everything below came out of the three-reviewer panel: each one is a case an
# earlier version of this file was missing, and each fails on the build that
# shipped before the panel ran.
sec "14. cases the reviewers found missing"

# (a) F-2: a flow that deliberately routes BACK into a fan-out must re-run
# every member. Reusing finished members there is not thrift, it is skipping
# work the flow explicitly asked for a second time. The first attempt at the
# F-2 fix mirrored completed members onto the next visit unconditionally and
# broke exactly this.
mkdir -p c14 && (
cd c14 || exit
# The route must lead back to the FAN-OUT ITSELF, or the case is not tested.
# Pass 1: fan runs, gate fails, run stops. Resume: gate now says "again" and
# routes to fan, which must run BOTH members a second time - the flow asked
# for another lap. Third time through, gate ends the run.
cat > flow.yaml <<'YAML'
name: routeback
steps:
  - id: fan
    max_parallel: 2
    max_visits: 4
    parallel:
      - id: m1
        cmd: ["sh", "-c", "echo m1 >> ../tally.txt; echo o1"]
      - id: m2
        cmd: ["sh", "-c", "echo m2 >> ../tally.txt; echo o2"]
  - id: gate
    max_visits: 4
    cmd: ["sh", "-c", "if [ ! -f ../t1 ]; then touch ../t1; exit 5; elif [ ! -f ../t2 ]; then touch ../t2; echo again; else echo done; fi"]
    route:
      - when_last_line_is: "again"
        goto: fan
      - goto: end
YAML
"$SFH" run flow.yaml --runs-dir runs -q >/dev/null 2>&1
rd="$(ls -d runs/*/ 2>/dev/null | head -1 | sed 's:/*$::')"
[ -n "$rd" ] && "$SFH" run flow.yaml --resume "$rd" --runs-dir runs -q >/dev/null 2>&1
printf '%s %s\n' \
  "$(if [ -f ../tally.txt ]; then grep -c '^m1$' ../tally.txt | head -1; else echo 0; fi)" \
  "$(if [ -f ../tally.txt ]; then grep -c '^m2$' ../tally.txt | head -1; else echo 0; fi)" \
  > ../c14.verdict
)
r1=""; r2=""
[ -f c14.verdict ] && read -r r1 r2 < c14.verdict
if [ "${r1:-0}" = "2" ] && [ "${r2:-0}" = "2" ]; then
  ok "F-2: a route back INTO the fan-out re-runs every member, as the flow asked"
else
  bad "F-2: a deliberate loop back into the fan-out ran m1=$r1 m2=$r2, expected 2 2 (members wrongly skipped as 'already done')"
fi

# (b) F-7: --force-resume waives the FINGERPRINT check. It must not also make
# the run dir's own claim about session access authoritative.
sess_flow s14.yaml t14 read full
"$SFH" run s14.yaml --runs-dir r14 -q >/dev/null 2>&1
rd14="$(newest r14)"
if [ -z "$rd14" ]; then bad "F-7(force): could not set up the run"; else
  sed -e 's/"access":"read"/"access":"full"/g' \
      "$rd14/log.jsonl" > "$rd14/l.new" && mv "$rd14/l.new" "$rd14/log.jsonl"
  "$SFH" run s14.yaml --resume "$rd14" --force-resume --runs-dir r14 -q >/dev/null 2>e14.txt
  if blocked_by_access e14.txt
    then ok  "F-7: --force-resume does not make a forged access:full authoritative"
    else bad "F-7: FAIL-OPEN - --force-resume trusted the log's own access claim"; fi
fi

# (c) F-1: a forged `sfh_version: 0.x` must not launder a CORRUPTED level. The
# 0.x allowance exists for logs with no access key at all.
sess_flow s14b.yaml t14b read read
"$SFH" run s14b.yaml --runs-dir r14b -q >/dev/null 2>&1
rd14b="$(newest r14b)"
if [ -z "$rd14b" ]; then bad "F-1(0.x+bogus): could not set up the run"; else
  sed -e 's/"access":"read"/"access":"bogus"/g' \
      "$rd14b/log.jsonl" > "$rd14b/l.new" && mv "$rd14b/l.new" "$rd14b/log.jsonl"
  sed -e 's/"sfh_version": *"[^"]*"/"sfh_version":"0.9.0"/' \
      "$rd14b/meta.json" > "$rd14b/m.new" && mv "$rd14b/m.new" "$rd14b/meta.json"
  "$SFH" run s14b.yaml --resume "$rd14b" --runs-dir r14b -q >/dev/null 2>e14b.txt
  if blocked_by_access e14b.txt
    then ok  "F-1: claiming to be a 0.x run does not launder a corrupted access level"
    else bad "F-1: FAIL-OPEN - 0.x + a bogus level was filled in like an honest old run"; fi
fi

# (d) F-10: each honesty field must be checked on its own, and the two writers
# emit different sets - aggregate_end has no timed_out/interrupted at all, so
# demanding them marked every honestly written fan-out as not-a-success.
for field in timed_out interrupted; do
  rm -rf "r10_$field" t10_$field
  cp f10.yaml "f10_$field.yaml"
  sed -i "s/^name: f10-resume/name: f10-$field/" "f10_$field.yaml" 2>/dev/null || true
  sed -i "s/t10r/t10_$field/" "f10_$field.yaml" 2>/dev/null || true
  "$SFH" run "f10_$field.yaml" --runs-dir "r10_$field" -q >/dev/null 2>&1
  rdf="$(newest "r10_$field")"
  if [ -z "$rdf" ]; then bad "F-10($field): could not set up the run"; continue; fi
  sed -e "s/,\"$field\":false//g" -e "s/\"$field\":false,//g" \
      "$rdf/log.jsonl" > "$rdf/l.new" && mv "$rdf/l.new" "$rdf/log.jsonl"
  outf="$("$SFH" run "f10_$field.yaml" --resume "$rdf" --runs-dir "r10_$field" -q 2>/dev/null)"
  case "$outf" in
    *"did not complete"*) ok  "F-10: dropping only '$field' is still not a success" ;;
    *CLEANTEXT*)          bad "F-10: FAIL-OPEN - dropping only '$field' passed as a success" ;;
    *)                    ok  "F-10: dropping only '$field' produced no restored output" ;;
  esac
done

# (e) F-10, the other direction: a fan-out that really did succeed must come
# back as a success. aggregate_end never writes timed_out/interrupted, so a
# loader demanding them treated every completed group as failed.
mkdir -p c14e && (
cd c14e || exit
cat > flow.yaml <<'YAML'
name: aggok
steps:
  - id: fan
    parallel:
      - id: q1
        cmd: ["echo", "AGGCLEAN-1"]
      - id: q2
        cmd: ["echo", "AGGCLEAN-2"]
  - id: gate
    cmd: ["sh", "-c", "if [ -f ../t14e ]; then echo ok; else touch ../t14e; exit 5; fi"]
  - id: use
    cmd: ["echo", "{{steps.fan.output}}"]
YAML
"$SFH" run flow.yaml --runs-dir runs -q >/dev/null 2>&1
rd="$(ls -d runs/*/ 2>/dev/null | head -1 | sed 's:/*$::')"
[ -n "$rd" ] && "$SFH" run flow.yaml --resume "$rd" --runs-dir runs -q > ../c14e.out 2>/dev/null
)
if [ -f c14e.out ] && grep -q 'AGGCLEAN-1' c14e.out && ! grep -q 'did not complete' c14e.out
  then ok  "F-10: a fan-out that really succeeded restores as a success"
  else bad "F-10: a successfully completed fan-out came back marked as failed"; fi

# (f) F-4: a 0.x run whose flow has a string-form cmd template must actually
# RESUME, not just pass validation with a warning and then be refused a step
# later by the same rule.
mkdir -p c14f/run && (
cd c14f || exit
cat > f.yaml <<'YAML'
name: legacycmd
steps:
  - id: a
    cmd: ["echo", "HELLO"]
  - id: b
    cmd: "echo got-{{steps.a.output}}"
YAML
printf '{"sfh_version":"0.9.0","flow":"f.yaml","flow_fingerprint":"x","name":"legacycmd","started_utc":"20250101-000000","os":"linux","vars":{},"tools":{},"resumed":false}' > run/meta.json
{
  printf '{"ts":"20250101-000000","event":"run_start","sfh_version":"0.9.0","resumed":false,"flow_fingerprint":"x"}\n'
  printf '{"ts":"20250101-000001","event":"step_end","step":"a","parent":null,"visit":1,"exit":0,"timed_out":false,"interrupted":false,"attempts":1,"dur_ms":1,"output_chars":5,"output_hash":"x","chain_file":"a.chain.txt","out_file":"a.out.txt","cmd":"echo HELLO","session":null}\n'
} > run/log.jsonl
echo "HELLO" > run/a.chain.txt
echo "HELLO" > run/a.out.txt
"$SFH" run f.yaml --resume run --force-resume -q > ../c14f.out 2> ../c14f.err
)
if grep -q 'got-HELLO' c14f.out 2>/dev/null
  then ok  "F-4: a 0.x run with a string-cmd template resumes and runs the step"
  else bad "F-4: the legacy allowance stops at validate - the step is still refused"; fi
if grep -q 'legacy resume' c14f.err 2>/dev/null
  then ok  "F-4: the relaxation is announced rather than silent"
  else bad "F-4: the legacy allowance is applied with no warning"; fi

# --------------------------------------------------------- 15. line endings
# A flow file checked out with CRLF and one checked out with LF are the same
# flow. Fingerprinting the raw bytes made them two different versions, so a run
# dir could not survive a re-checkout under a different core.autocrlf, let
# alone a move between a Windows and a Unix working copy.
sec "15. LEGIT / the same flow with CRLF and LF must resume interchangeably"

mkdir -p c15 && (
cd c15 || exit
printf 'name: crlf\nsteps:\n  - id: a\n    cmd: ["echo", "first"]\n  - id: b\n    cmd: ["sh", "-c", "if [ -f ../t15 ]; then echo ok; else touch ../t15; exit 5; fi"]\n' > flow.yaml
"$SFH" run flow.yaml --runs-dir runs -q >/dev/null 2>&1
rd="$(ls -d runs/*/ 2>/dev/null | head -1 | sed 's:/*$::')"
# Same content, CRLF endings - what a Windows checkout produces.
printf 'name: crlf\r\nsteps:\r\n  - id: a\r\n    cmd: ["echo", "first"]\r\n  - id: b\r\n    cmd: ["sh", "-c", "if [ -f ../t15 ]; then echo ok; else touch ../t15; exit 5; fi"]\r\n' > flow.yaml
if [ -n "$rd" ]; then
  "$SFH" run flow.yaml --resume "$rd" --runs-dir runs -q >/dev/null 2>../c15.err
  echo "$?" > ../c15.rc
else
  echo "setup" > ../c15.rc
fi
)
rc15="$(cat c15.rc 2>/dev/null || echo setup)"
if [ "$rc15" = "setup" ]; then
  bad "CRLF: could not set up the run"
elif grep -q "different version" c15.err 2>/dev/null; then
  bad "CRLF: the same flow with different line endings reads as a changed flow"
elif [ "$rc15" = "0" ]; then
  ok "CRLF: a run made from an LF checkout resumes against a CRLF checkout"
else
  bad "CRLF: the resume failed (exit $rc15) - see c15.err"
fi

# ---------------------------------------------------------------- summary
sec "summary"
echo "  pass $pass   fail $fail   skip $skip"
[ "$fail" -eq 0 ] || exit 1
exit 0
