"""Times each pinned client against a ready bctl session: attach, navigate, accessibility tree, screenshot, close.

Run through `BCTL_INTEROP_SUITE=measure tests/interop/run.sh`, which supplies the session, its CDP URL and the fixture.
Rounds alternate the clients. Each operation is timed from the host as the client's user sees it, so agent-browser's
times include starting its CLI. Prints a table, then one JSON line with every sample.
"""

import asyncio
import json
import os
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from pydantic_ai.messages import ModelResponse, ToolCallPart
from pydantic_ai.models.function import FunctionModel

from browser_use import Agent, Browser

SESSION, URL_FILE, FIXTURE_PORT = sys.argv[1], Path(sys.argv[2]), int(sys.argv[3])
ROUNDS = int(os.environ.get('BCTL_INTEROP_ROUNDS', '7'))
SITE = f'http://100.96.0.1:{FIXTURE_PORT}'
cdp_url = URL_FILE.read_text().strip()
samples: dict[str, dict[str, list[float]]] = {}


def record(client: str, op: str, value: float) -> None:
	samples.setdefault(client, {}).setdefault(op, []).append(value)


def rss_mib(pid: int) -> float:
	out = subprocess.run(['ps', '-o', 'rss=', '-p', str(pid)], capture_output=True, text=True).stdout.strip()
	return int(out) / 1024 if out else 0.0


def agent_browser(session: str, *args: str) -> float:
	"""Runs one agent-browser command; returns its wall time in ms and fails loudly if it did not succeed."""
	env = {**os.environ, 'AGENT_BROWSER_CDP': cdp_url, 'AGENT_BROWSER_SESSION': session, 'AGENT_BROWSER_NO_AUTO_DIALOG': '1'}
	start = time.perf_counter()
	done = subprocess.run([os.environ['AGENT_BROWSER'], *args], capture_output=True, text=True, timeout=90, env=env)
	elapsed = (time.perf_counter() - start) * 1000
	if done.returncode != 0:
		raise RuntimeError(f'agent-browser {" ".join(args)}: {done.stderr.strip()[-200:]}')
	return elapsed


def agent_browser_round(n: int, scratch: Path) -> None:
	c, s = 'agent-browser', f'm{os.getpid()}r{n}'
	record(c, 'attach', agent_browser(s, 'eval', '1'))
	record(c, 'navigate', agent_browser(s, 'open', SITE + '/form'))
	record(c, 'ax tree', agent_browser(s, 'snapshot'))
	record(c, 'screenshot', agent_browser(s, 'screenshot', str(scratch / f'ab{n}.png')))
	socket_dir = Path(os.environ.get('AGENT_BROWSER_SOCKET_DIR') or Path.home() / '.agent-browser')
	pid = int((socket_dir / f'{s}.pid').read_text().strip())
	record(c, 'daemon RSS MiB', rss_mib(pid))
	record(c, 'close', agent_browser(s, 'close'))


TIMED = (
	"import time, json\nT = {}\n"
	"t = time.perf_counter(); new_tab({form!r}); wait_for_load(); T['navigate'] = (time.perf_counter() - t) * 1000\n"
	"t = time.perf_counter(); cdp('Accessibility.getFullAXTree'); T['ax tree'] = (time.perf_counter() - t) * 1000\n"
	"t = time.perf_counter(); capture_screenshot({shot!r}); T['screenshot'] = (time.perf_counter() - t) * 1000"
)


async def browser_use_round(n: int, scratch: Path) -> None:
	c = 'browser-use'
	cells = ["T0 = 1", TIMED.replace('{form!r}', repr(SITE + '/form')).replace('{shot!r}', repr(str(scratch / f'bu{n}.png')))]
	turns = iter([[ToolCallPart('python', {'code': cell})] for cell in cells] + [[ToolCallPart('finish_from_repl', {'success': True, 'expression': 'json.dumps(T)'})]])
	stamps: list[float] = []

	def model(messages, info):
		stamps.append(time.perf_counter())
		return ModelResponse(parts=next(turns))

	browser = Browser(cdp_url=cdp_url)
	agent = Agent('measure', FunctionModel(model), browser=browser, workspace=tempfile.mkdtemp(prefix='measure-', dir=scratch))
	start = time.perf_counter()
	result = await agent.run()
	ended = time.perf_counter()
	timed = json.loads(result.final_result() or '{}')
	if set(timed) != {'navigate', 'ax tree', 'screenshot'}:
		raise RuntimeError(f'browser-use round {n}: {result.final_result()}')
	record(c, 'attach, first cell run', (stamps[1] - start) * 1000)
	for op, ms in timed.items():
		record(c, op, ms)
	record(c, 'run, start to finish', (ended - start) * 1000)
	import resource

	record(c, 'agent process peak RSS MiB', resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / (1 << 20 if sys.platform == 'darwin' else 1 << 10))


def p95(values: list[float]) -> float:
	ordered = sorted(values)
	return ordered[min(len(ordered) - 1, round(0.95 * (len(ordered) - 1)))]


async def main() -> None:
	scratch = Path(tempfile.mkdtemp(prefix='measure-'))
	for n in range(ROUNDS):
		if n % 2:
			agent_browser_round(n, scratch)
			await browser_use_round(n, scratch)
		else:
			await browser_use_round(n, scratch)
			agent_browser_round(n, scratch)
	print(f'{"client":<14} {"operation":<22} {"n":>3} {"median":>9} {"p95":>9} {"min":>9} {"max":>9}')
	for client, ops in samples.items():
		for op, values in ops.items():
			print(f'{client:<14} {op:<22} {len(values):>3} {statistics.median(values):>9.1f} {p95(values):>9.1f} {min(values):>9.1f} {max(values):>9.1f}')
	print(json.dumps({'rounds': ROUNDS, 'samples': samples}))


asyncio.run(main())
