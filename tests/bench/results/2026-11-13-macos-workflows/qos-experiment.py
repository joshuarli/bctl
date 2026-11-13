import re
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, 'tests/bench')
import macos_workflows as suite

active = None
initialize = suite.Trial.__init__
invoke = suite.invoke

def trial_init(self, *args, **kwargs):
    global active
    initialize(self, *args, **kwargs)
    active = self

def with_policy(argv, **kwargs):
    result = invoke(argv, **kwargs)
    if Path(str(argv[0])).name == 'agent-browser' and argv[1:3] == ['set', 'viewport'] and active.side == 'bctl':
        round_id = int(re.fullmatch(r'l(\d+)b\d+', active.path.name).group(1))
        if round_id % 4 in (1, 2):
            rows = [line.strip().split(None, 2) for line in suite.process_table().splitlines()]
            pids = [pid for pid, _, command in rows if Path(command.split()[0]).name == 'nanos-krun'
                    and str(active.path / 'bctl') in command]
            if len(pids) != 1:
                raise RuntimeError(f'expected one owned VMM, found {pids}')
            subprocess.run([shutil.which('taskpolicy'), '-l', '0', '-p', pids[0]], check=True)
            print(f'POLICY {active.path.name} latency tier 0', flush=True)
    return result

suite.Trial.__init__ = trial_init
suite.invoke = with_policy
sys.argv = ['macos_workflows.py', '--rounds', '10', '--resource-rounds', '0', '--workloads',
            'agent-browser', 'agent-browser-news', '--native-arg=--disable-gpu',
            '--out', '.work/tmp/macos-qos-balanced-experiment.json']
raise SystemExit(suite.main())
