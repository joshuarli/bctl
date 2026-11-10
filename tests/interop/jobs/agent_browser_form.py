"""Test job: the agent-browser recipe fills the fixture form, saves a screenshot, then holds HOLD seconds.

Arguments: URL NAME HOLD. Writes result.json to BCTL_OUTPUT_DIR.
"""

import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / 'recipes'))

from agent_browser_job import AgentBrowser, Failed  # noqa: E402

url, name, hold = sys.argv[1], sys.argv[2], float(sys.argv[3])
output = Path(os.environ['BCTL_OUTPUT_DIR'])
report = {'run': os.environ['BCTL_RUN'], 'success': False, 'result': None, 'errors': []}
try:
	with AgentBrowser(url) as ab:
		ab('fill', '#name', name)
		ab('click', '#submit')
		ab('wait', '--text', f'sent {name}')
		report['result'] = ab('get', 'text', '#out')
		ab('screenshot', str(output / 'form.png'))
		report['success'] = report['result'] == f'sent {name}'
		time.sleep(hold)
except Failed as error:
	report['errors'].append(str(error))
(output / 'result.json').write_text(json.dumps(report) + '\n')
print(json.dumps(report), flush=True)
sys.exit(0 if report['success'] else 1)
