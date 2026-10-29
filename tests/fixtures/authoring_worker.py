"""Disposable command worker for real bundled-flow route/context tests; no APIs."""
import json
from pathlib import Path
import sys

step, scenario = sys.argv[1:3]
prompt = sys.stdin.read()
state_file = Path('candidate.json')
state = json.loads(state_file.read_text()) if state_file.exists() else {'mechanical': False, 'semantic': False}
counts_file = Path('counts.json')
counts = json.loads(counts_file.read_text()) if counts_file.exists() else {}
counts[step] = counts.get(step, 0) + 1
counts_file.write_text(json.dumps(counts))

def save():
    state_file.write_text(json.dumps(state))

def need(marker):
    if marker not in prompt:
        print('missing input evidence: ' + marker, file=sys.stderr)
        sys.exit(91)

if step in ('plan', 'plan_chunks', 'analyze_trace'):
    print('one coherent fixture chunk')
elif step in ('build', 'build_chunks', 'write_eval', 'add_eval') or (step == 'implement' and (scenario.startswith('loop-') or scenario in ('stalled', 'infra'))):
    if scenario == 'loop-pass':
        state = {'mechanical': True, 'semantic': True}
    save()
    print('fixture artifact written')
elif step == 'verify':
    print('VERIFY-EVIDENCE expected mechanical=true observed=' + str(state['mechanical']))
    if scenario == 'infra':
        sys.exit(129)
    sys.exit(0 if state['mechanical'] and scenario != 'stalled' else 2)
elif step in ('reproduce', 'prove_failure'):
    print('REPRO-EVIDENCE expected mechanical=true observed=' + str(state['mechanical']))
    if scenario.startswith('repro-error-'):
        sys.exit(int(scenario.rsplit('-', 1)[1]))
    if scenario == 'not-reproduced':
        sys.exit(0)
    # Confirm actual fixture failure, not an arbitrary command exit.
    sys.exit(20 if not state['mechanical'] else 0)
elif step in ('focused', 'targeted_eval', 'regression', 'full_regression'):
    print('CHECK-EVIDENCE expected mechanical=true observed=' + str(state['mechanical']))
    if (scenario == 'focused-error' and step in ('focused', 'targeted_eval')) or (scenario == 'regression-error' and step in ('regression', 'full_regression')):
        sys.exit(2)
    sys.exit(0 if state['mechanical'] else 20)
elif step in ('improve', 'repair', 'fix', 'implement'):
    if scenario.startswith('loop-') or scenario in ('stalled', 'infra'):
        need('VERIFY-EVIDENCE')
        if state['mechanical'] and scenario != 'stalled':
            need('EVAL-BLOCKER')
            state['semantic'] = True
        else:
            state['mechanical'] = True
    else:
        need('REPRO-EVIDENCE')
        state['mechanical'] = True
        state['semantic'] = True
    save()
elif step in ('evaluate', 'review'):
    if scenario.startswith('loop-') or scenario == 'stalled':
        need('VERIFY-EVIDENCE')
    if scenario == 'loop-malformed':
        print('perhaps acceptable')
    elif state['mechanical'] and state['semantic']:
        print('REVIEW: PASS')
    else:
        print('EVAL-BLOCKER: semantic criterion missing\nREVIEW: REVISE')
elif step == 'handoff':
    need('VERIFY-EVIDENCE')
    print('handoff: ' + prompt)
else:
    raise RuntimeError('unexpected fixture step: ' + step)
