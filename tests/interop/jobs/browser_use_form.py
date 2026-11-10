"""Test job: the browser-use recipe with a scripted model fills the fixture form, then holds HOLD seconds.

Arguments: URL NAME HOLD. Every proxy variable points at a closed port and no Browser Use key is set, so the run
succeeds only if neither the model nor the browser needs Browser Use Cloud.
"""

import asyncio
import os
import sys
from pathlib import Path

for name in ('HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy'):
	os.environ[name] = 'http://127.0.0.1:9'
os.environ['NO_PROXY'] = os.environ['no_proxy'] = 'localhost'
os.environ.pop('BROWSER_USE_API_KEY', None)
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / 'recipes'))

from pydantic_ai.messages import ModelResponse, ToolCallPart  # noqa: E402
from pydantic_ai.models.function import FunctionModel  # noqa: E402

import browser_use_job  # noqa: E402

url, name, hold = sys.argv[1], sys.argv[2], float(sys.argv[3])
cell = (
	f'new_tab({url!r}); wait_for_load()\n'
	f"fill_input('#name', {name!r})\n"
	"r = js(\"(() => { const b = document.querySelector('#submit').getBoundingClientRect(); return [b.x + b.width / 2, b.y + b.height / 2] })()\")\n"
	'click_at_xy(*r)\n'
	"for _ in range(50):\n\tif js(\"document.querySelector('#out').textContent\"): break\n\twait(0.1)\n"
	f'wait({hold})'
)
turns = iter([
	[ToolCallPart('python', {'code': cell})],
	[ToolCallPart('finish_from_repl', {'success': True, 'expression': "js(\"document.querySelector('#out').textContent\")"})],
])
sys.exit(asyncio.run(browser_use_job.run('fill the form', FunctionModel(lambda messages, info: ModelResponse(parts=next(turns))))))
