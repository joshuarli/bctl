"""Runs the released clients against a real bctl session; tests/interop/run.sh starts it with the session up.

Arguments: SESSION CDP_URL_FILE FIXTURE_PORT. Environment: BCTL, BCTL_HOME, BCTL_RUNTIME and AGENT_BROWSER from
run.sh. Prints one PASS or FAIL line per check, then the receipts as JSON, and exits 1 if any check failed. The
capability URL is read from a file and never printed: every line passes through redact().
"""

import asyncio
import http.client
import json
import os
import re
import secrets
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

os.environ['ANONYMIZED_TELEMETRY'] = 'false'

from pydantic_ai.messages import ModelResponse, ToolCallPart  # noqa: E402
from pydantic_ai.models.function import FunctionModel  # noqa: E402

from browser_use import Agent, Browser  # noqa: E402

SESSION, URL_FILE, FIXTURE_PORT = sys.argv[1], Path(sys.argv[2]), int(sys.argv[3])
REPEATS = 10
SITE = f'http://100.96.0.1:{FIXTURE_PORT}'
OTHER_SITE = f'http://[::ffff:100.96.0.1]:{FIXTURE_PORT}'
PNG = b'\x89PNG\r\n\x1a\n'
failures = 0
cdp_url = URL_FILE.read_text().strip()


def redact(text: str) -> str:
	text = text.replace(cdp_url, '<cdp-url>')
	return re.sub(r'[A-Za-z0-9_-]{16,}\.localhost', '<token>.localhost', text)


def check(consumer: str, name: str, ok: bool, detail: object = '') -> bool:
	global failures
	failures += not ok
	print(f'{"PASS" if ok else "FAIL"}  {consumer:<13} {name:<34} {redact(str(detail))[:300]}', flush=True)
	return ok


def records() -> list[str]:
	return json.load(urllib.request.urlopen(f'http://127.0.0.1:{FIXTURE_PORT}/_records', timeout=10))


def bctl(*args: str) -> subprocess.CompletedProcess:
	if args and args[0] == 'down':
		home = Path(os.environ['BCTL_HOME'])
		for name in ('console.log', 'supervisor.log'):
			for log in (home / 'machines').glob(f'*/{name}'):
				try:
					evidence = home / 'diagnostics'
					evidence.mkdir(mode=0o700, exist_ok=True)
					shutil.copyfile(log, evidence / f'{log.parent.name}.{name}')
				except OSError as error:
					print(redact(f'cannot preserve {log}: {error}'), file=sys.stderr)
	return subprocess.run([os.environ['BCTL'], *args], capture_output=True, text=True, timeout=120)


def session_ready() -> bool:
	listed = json.loads(bctl('inspect', SESSION, '--json').stdout)
	return listed['sessions'][0]['state'] == 'ready'


gateway_port = urllib.parse.urlsplit(cdp_url).port


def client_sockets() -> int:
	"""Established host connections to the gateway port, counted at the client end."""
	port = gateway_port
	out = subprocess.run(['lsof', '-nP', f'-iTCP:{port}', '-sTCP:ESTABLISHED', '-Fn'], capture_output=True, text=True).stdout
	return sum(1 for line in out.splitlines() if line.startswith('n') and line.endswith(f'->127.0.0.1:{port}'))


def get(url: str, headers: dict[str, str] | None = None, host: str | None = None) -> tuple[int, bytes]:
	"""An HTTP GET to the gateway's address with a chosen Host header, bypassing name resolution."""
	u = urllib.parse.urlsplit(url)
	conn = http.client.HTTPConnection('127.0.0.1', u.port, timeout=10)
	conn.request('GET', u.path or '/', headers={'Host': host or u.netloc, **(headers or {})})
	response = conn.getresponse()
	return response.status, response.read()


def gateway_checks() -> str:
	version = json.load(urllib.request.urlopen(f'{cdp_url}/json/version', timeout=10))
	ws = urllib.parse.urlsplit(version['webSocketDebuggerUrl'])
	base = urllib.parse.urlsplit(cdp_url)
	check('gateway', 'discovery reports Helium', version.get('Browser', '').startswith('Helium/'), version.get('Browser'))
	check('gateway', 'browser WS URL uses the capability', (ws.scheme, ws.netloc) == ('ws', base.netloc) and
		  ws.path.startswith('/devtools/browser/'), ws.path)  # fmt: skip
	status, _ = get(f'{cdp_url}/json/version', {'Origin': 'http://evil.example'})
	check('gateway', 'browser Origin is rejected', status in (401, 403), status)
	status, _ = get(f'{cdp_url}/json/version', host=f'127.0.0.1:{base.port}')
	check('gateway', 'bare loopback Host is rejected', status != 200, status)
	forged = f'http://{secrets.token_hex(16)}.localhost:{base.port}'
	status, _ = get(f'{forged}/json/version')
	check('gateway', 'unknown token is rejected', status != 200, status)
	return version['Browser']


def guarded(cell: str) -> str:
	"""The cell, recording its exception in R['errors'] so a failed check can show it."""
	body = '\n'.join('\t' + line for line in cell.splitlines())
	return f"try:\n{body}\nexcept Exception as e:\n\tR.setdefault('errors', []).append(repr(e)[:300])"


def script(*cells: str, finish: str) -> FunctionModel:
	"""A FunctionModel that runs the cells in order, then finishes with the REPL value of `finish`."""
	turns = [[ToolCallPart('python', {'code': c})] for c in cells]
	turns.append([ToolCallPart('finish_from_repl', {'success': True, 'expression': finish})])
	calls = iter(turns)
	return FunctionModel(lambda messages, info: ModelResponse(parts=next(calls)))


async def agent_run(cells: list[str], finish: str, **browser: object) -> tuple[str | None, Browser]:
	mode = browser.pop('mode', None)
	b = Browser(cdp_url=cdp_url, **browser)
	workspace = tempfile.mkdtemp(prefix='interop-')
	agent = Agent('interop check', script(*cells, finish=finish), browser=b, workspace=workspace, **({'mode': mode} if mode else {}))
	result = await agent.run()
	return result.final_result(), b


CLICK = "r = js(\"(() => { const b = document.querySelector('{sel}').getBoundingClientRect(); return [b.x + b.width / 2, b.y + b.height / 2] })()\"); click_at_xy(*r)"


def click(selector: str) -> str:
	return CLICK.replace('{sel}', selector)


def browser_use_cells(mode: str, shot: Path) -> list[str]:
	cells = [
		"import json, subprocess\nR = {}\nv = cdp('Browser.getVersion')\nR['product'] = v['product']\n"
		"R['targets'] = len(cdp('Target.getTargets')['targetInfos'])",
		f"new_tab({SITE + '/form'!r}); wait_for_load()\nfill_input('#name', 'Ada {mode}')\n{click('#submit')}\n"
		"for _ in range(50):\n\tif js(\"document.querySelector('#out').textContent\"): break\n\twait(0.1)\n"
		"R['form'] = js(\"document.querySelector('#out').textContent\")",
		"ax = cdp('Accessibility.getFullAXTree')['nodes']\n"
		"R['ax'] = any((n.get('name') or {}).get('value') == 'Submit' and (n.get('role') or {}).get('value') == 'button' for n in ax)",
		f"p = capture_screenshot({str(shot)!r})\nimport pathlib\nb = pathlib.Path(p).read_bytes()\nR['png'] = [b[:8] == {PNG!r}, len(b)]",
		f"goto_url({SITE + '/dialog'!r}); wait_for_load()\ncdp('Page.enable')\n"
		"js(\"setTimeout(() => document.querySelector('#ask').click(), 0)\")\n"
		"R['dialog_seen'] = None\n"
		"for _ in range(50):\n"
		"\ttry:\n\t\tR['dialog_seen'] = page_info().get('dialog', {}).get('type')\n"
		"\texcept RuntimeError as e:\n\t\tR['dialog_seen'] = 'confirm' if 'JavaScript confirm is open' in str(e) else repr(e)\n"
		"\tif R['dialog_seen']: break\n\twait(0.1)\n"
		+ ("cdp('Page.handleJavaScriptDialog', accept=True)\n" if mode == 'default' else '')
		+ "for _ in range(30):\n\tif js('document.title') != 'dialog': break\n\twait(0.1)\nR['dialog'] = js('document.title')",
		f"goto_url({SITE + '/popup'!r}); wait_for_load()\n"
		"popped = lambda: sum(t.get('url', '').endswith('/popped') for t in list_tabs())\nbefore = popped()\n"
		f"{click('#pop')}\n"
		"for _ in range(50):\n\tif popped() > before: break\n\twait(0.1)\n"
		"R['popups'] = [before, popped()]",
		f"goto_url({SITE + '/frames'!r}); wait_for_load(); wait(1)\n"
		"R['same_frame'] = js(\"document.querySelector('#same').contentDocument.title\")\n"
		"t = iframe_target('f=cross')\nR['cross_frame'] = js('document.title', target_id=t) if t else None\n"
		"if t: js(\"document.querySelector('#b').click()\", target_id=t)",
	]
	if mode == 'ultrafast':
		cells.append(f"goto_url({SITE + '/form'!r}); wait_for_load()\nR['bu_state'] = 'Submit' in str(bu.state())")
	return [cells[0], *map(guarded, cells[1:])]


async def browser_use_checks(scratch: Path) -> None:
	for mode in ('default', 'ultrafast'):
		before = records()
		value, _ = await agent_run(browser_use_cells(mode, scratch / f'bu-{mode}.png'), 'json.dumps(R)', mode=mode)
		r = json.loads(value or '{}')
		c = f'browser-use/{mode}'[:13]
		check(c, 'Browser.getVersion and targets', str(r.get('product', '')).startswith('Helium/') and r.get('targets', 0) > 0, r.get('product'))
		check(c, 'form fill and click', r.get('form') == f'sent Ada {mode}', r.get('form'))
		check(c, 'fixture saw the submission', f'name=Ada%20{mode}' in records()[len(before):], records()[len(before):])
		check(c, 'accessibility tree', r.get('ax') is True)
		check(c, 'screenshot bytes', r.get('png', [False, 0])[0] and r['png'][1] > 1000, r.get('png'))
		if mode == 'default':
			check(c, 'dialog seen and accepted', r.get('dialog_seen') == 'confirm' and r.get('dialog') == 'confirmed', [r.get('dialog_seen'), r.get('dialog')])
		else:
			check(c, 'dialog accepted automatically', r.get('dialog') == 'confirmed', r.get('dialog'))
		pops = r.get('popups') or [0, 0]
		check(c, 'popup target', pops[1] == pops[0] + 1, pops)
		check(c, 'same-origin iframe', r.get('same_frame') == 'inner-same', r.get('same_frame'))
		check(c, 'cross-origin iframe target', r.get('cross_frame') == 'inner-cross', r.get('cross_frame'))
		if mode == 'ultrafast':
			check(c, 'bu.state accessibility', r.get('bu_state') is True)
		check(c, 'cells ran without error', not r.get('errors'), r.get('errors'))
	check('browser-use', 'session survives client stop', session_ready())

	guard_cells = [
		"import subprocess, json\nR = {}",
		f"goto_url({SITE + '/form'!r}); wait_for_load()\nR['allowed'] = js('document.title')",
		f"js(\"location.href = {OTHER_SITE + '/form'!r}\")\nwait(2)\nR['blocked'] = js('document.title')",
	]
	sockets_before = client_sockets()
	value, guarded = await agent_run(guard_cells, 'json.dumps(R)', allowed_domains=['100.96.0.1'], keep_alive=True)
	r = json.loads(value or '{}')
	during = client_sockets()
	await guarded.stop()
	time.sleep(1)
	after = client_sockets()
	check('browser-use', 'domain guard allows the site', r.get('allowed') == 'form', r.get('allowed'))
	check('browser-use', 'domain guard blocks other site', r.get('blocked') != 'form', r.get('blocked'))
	check('browser-use', 'guard adds a second CDP socket', during >= sockets_before + 2, f'before={sockets_before} during={during}')
	check('browser-use', 'guard sockets close on stop', after == sockets_before, f'before={sockets_before} after={after}')

	ok = 0
	for i in range(REPEATS):
		value, _ = await agent_run([f"R = cdp('Browser.getVersion')['product'] + ' {i}'"], 'R')
		ok += bool(value and value.startswith('Helium/') and value.endswith(f' {i}'))
	check('browser-use', f'{REPEATS} attach/detach cycles', ok == REPEATS, f'{ok}/{REPEATS}')
	check('browser-use', 'no client socket left open', client_sockets() == 0, client_sockets())

	proxied = {'HTTP_PROXY': 'http://127.0.0.1:9', 'HTTPS_PROXY': 'http://127.0.0.1:9', 'ALL_PROXY': 'http://127.0.0.1:9',
			   'NO_PROXY': 'localhost'}  # fmt: skip
	saved = {k: os.environ.get(k) for k in proxied}
	os.environ.update(proxied)
	try:
		value, _ = await agent_run(["R = cdp('Browser.getVersion')['product']"], 'R')
		check('browser-use', 'proxy set, NO_PROXY=localhost', bool(value and value.startswith('Helium/')), value)
	finally:
		for k, v in saved.items():
			os.environ.pop(k) if v is None else os.environ.__setitem__(k, v)


def agent_browser(session: str, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
	environment = {**os.environ, 'AGENT_BROWSER_CDP': cdp_url, 'AGENT_BROWSER_SESSION': session, 'AGENT_BROWSER_NO_AUTO_DIALOG': '1',
				   **(env or {})}  # fmt: skip
	return subprocess.run([os.environ['AGENT_BROWSER'], *args], capture_output=True, text=True, timeout=90, env=environment)


def out(p: subprocess.CompletedProcess) -> str:
	return (p.stdout.strip().splitlines() or [''])[-1] if p.returncode == 0 else f'exit {p.returncode}: {p.stderr.strip()[-200:]}'


def steps(session: str, *commands: tuple[str, ...]) -> list[str]:
	"""Runs agent-browser commands in order; returns what each printed, for the detail of the check that follows."""
	return [f'{" ".join(c)} -> {out(agent_browser(session, *c))}' for c in commands]


def agent_browser_checks(scratch: Path) -> None:
	c, s = 'agent-browser', f'interop{os.getpid()}'
	before = records()
	attached = out(agent_browser(s, 'eval', 'document.visibilityState'))
	print(f'NOTE  {c:<13} {"tab bound on attach":<34} visibilityState={attached}', flush=True)
	agent_browser(s, 'tab', 'new', SITE + '/form')
	visible = out(agent_browser(s, 'eval', 'document.visibilityState'))
	check(c, 'tab new is visible', visible == '"visible"', visible)
	log = steps(s, ('fill', '#name', 'Lin'), ('click', '#submit'), ('wait', '--text', 'sent Lin'))
	text = out(agent_browser(s, 'get', 'text', '#out'))
	check(c, 'form fill and click', text == 'sent Lin', f'{text!r} after {log}')
	check(c, 'fixture saw the submission', 'name=Lin' in records()[len(before):], records()[len(before):])
	snapshot = agent_browser(s, 'snapshot')
	check(c, 'accessibility snapshot', snapshot.returncode == 0 and 'button "Submit"' in snapshot.stdout, snapshot.stdout[:200])
	shot = scratch / 'ab.png'
	agent_browser(s, 'screenshot', str(shot))
	data = shot.read_bytes() if shot.exists() else b''
	check(c, 'screenshot bytes', data[:8] == PNG and len(data) > 1000, len(data))
	ua = out(agent_browser(s, 'eval', 'navigator.userAgent'))
	check(c, 'page JavaScript', 'Chrome/' in ua, ua)

	log = steps(s, ('open', SITE + '/dialog'), ('click', '#ask'), ('dialog', 'status'), ('dialog', 'accept'))
	title = out(agent_browser(s, 'eval', 'document.title'))
	check(c, 'dialog accepted', title == '"confirmed"', f'{title} after {log}')

	agent_browser(s, 'open', SITE + '/popup')
	def popped() -> int:
		listed = json.loads(agent_browser(s, 'tab', 'list', '--json').stdout or '{}')
		return sum(t.get('url', '').endswith('/popped') for t in listed.get('data', {}).get('tabs', []))

	before_popups = popped()
	agent_browser(s, 'click', '#pop')
	time.sleep(1)
	after_popups = popped()
	check(c, 'popup target', after_popups == before_popups + 1, f'{before_popups} -> {after_popups}')

	before = records()
	agent_browser(s, 'open', SITE + '/frames')
	agent_browser(s, 'wait', '--load', 'load')
	frames = re.findall(r'Iframe \[ref=(e\d+)\]', agent_browser(s, 'snapshot').stdout)
	cross = f'snapshot found {len(frames)} iframes'
	if len(frames) == 2:
		agent_browser(s, 'frame', f'@{frames[0]}')
		agent_browser(s, 'click', '#b')
		agent_browser(s, 'frame', 'main')
		agent_browser(s, 'frame', f'@{frames[1]}')
		agent_browser(s, 'click', '#b')
		cross = out(agent_browser(s, 'get', 'text', '#b'))
		agent_browser(s, 'frame', 'main')
	time.sleep(0.5)
	seen = records()[len(before):]
	check(c, 'same-origin iframe click', 'frame=same' in seen, seen)
	check(c, 'cross-origin iframe click', 'frame=cross' in seen and cross == 'clicked', f'{seen} text={cross}')
	agent_browser(s, 'close')
	check(c, 'session survives client close', session_ready())

	ok = 0
	for i in range(REPEATS):
		si = f'{s}r{i}'
		opened = agent_browser(si, 'open', SITE + '/form')
		title = out(agent_browser(si, 'get', 'title'))
		closed = agent_browser(si, 'close')
		ok += opened.returncode == 0 and title == 'form' and closed.returncode == 0
	check(c, f'{REPEATS} attach/detach cycles', ok == REPEATS, f'{ok}/{REPEATS}')
	time.sleep(1)
	check(c, 'no client socket left open', client_sockets() == 0, client_sockets())

	proxied = {'HTTP_PROXY': 'http://127.0.0.1:9', 'HTTPS_PROXY': 'http://127.0.0.1:9', 'ALL_PROXY': 'http://127.0.0.1:9',
			   'NO_PROXY': 'localhost'}  # fmt: skip
	sp = f'{s}proxy'
	title = out(agent_browser(sp, 'open', SITE + '/form', env=proxied))
	agent_browser(sp, 'close', env=proxied)
	check(c, 'proxy set, NO_PROXY=localhost', 'form' in title, title)


HERE = Path(__file__).resolve().parent
TOKEN = re.compile(r'[0-9a-f]{48}\.localhost')


def write_jobs(scratch: Path) -> Path:
	"""A bctl.toml whose exec jobs run the recipes' test programs in this session, with its profile and image."""
	python, form = sys.executable, SITE + '/form'

	def job(name: str, program: str, who: str, hold: int) -> str:
		argv = json.dumps([python, str(HERE / 'jobs' / program), form, who, str(hold)])
		return f'[jobs.{name}]\nkind = "exec"\nbrowser = "interop"\nprofile = "{SESSION}"\nargv = {argv}\ntimeout = "3m"\n'

	config = scratch / 'bctl.toml'
	config.write_text(
		f'version = 1\n\n[browsers.interop]\nimage = "{os.environ["BCTL_IMAGE"]}"\n\n'
		+ job('bu-form', 'browser_use_form.py', 'Ada job', 0)
		+ job('ab-form', 'agent_browser_form.py', 'Lin job', 0)
		+ job('bu-hold', 'browser_use_form.py', 'Ada hold', 120)
		+ job('ab-hold', 'agent_browser_form.py', 'Lin hold', 120)
		+ job('bu-kill', 'browser_use_form.py', 'Ada kill', 120)
	)
	return config


def run_record(run: int) -> dict:
	return json.loads(bctl('runs', str(run), '--json').stdout)['runs'][0]


def start_job(config: Path, name: str) -> int:
	started = json.loads(bctl('run', name, '--session', SESSION, '--config', str(config), '--json').stdout)
	if started.get('error'):
		raise RuntimeError(f'bctl run {name}: {started["error"]}')
	return started['runs'][0]['id']


def wait_run(run: int, until, timeout: float = 180) -> dict:
	deadline = time.monotonic() + timeout
	while True:
		record = run_record(run)
		if until(record) or time.monotonic() > deadline:
			return record
		time.sleep(0.5)


def ended(record: dict) -> bool:
	return record['state'] in ('succeeded', 'failed', 'cancelled', 'interrupted')


def leftovers(marker: str) -> list[str]:
	"""Processes other than this one and the fixture whose command line contains `marker`."""
	out = subprocess.run(['ps', '-Ao', 'pid=,command='], capture_output=True, text=True).stdout
	return [line.strip() for line in out.splitlines() if marker in line and 'checks.py' not in line and 'fixture.py' not in line]


def leaks(run: int, record: dict) -> list[str]:
	"""Places a capability URL appears in: the run's log, its record, or its output files."""
	found = [where for where, text in (('log', bctl('logs', str(run)).stdout), ('record', json.dumps(record))) if TOKEN.search(text)]
	for path in Path(record['output_dir']).rglob('*'):
		if path.is_file() and TOKEN.search(path.read_bytes().decode(errors='replace')):
			found.append(str(path.relative_to(record['output_dir'])))
	return found


def controller_free() -> bool:
	"""Whether a newly minted URL attaches, which no other controller may hold; follows the gateway's current port."""
	global gateway_port
	fresh = bctl('cdp-url', SESSION).stdout.strip()
	gateway_port = urllib.parse.urlsplit(fresh).port
	status, _ = get(f'{fresh}/json/version')
	return status == 200


def job_checks(scratch: Path) -> None:
	config = write_jobs(scratch)
	venv = str(Path(sys.executable).parent.parent)
	for job, client, who in (('bu-form', 'browser-use', 'Ada job'), ('ab-form', 'agent-browser', 'Lin job')):
		c = f'job/{client}'[:13]
		before = records()
		run = start_job(config, job)
		record = wait_run(run, ended)
		result_file = Path(record['output_dir']) / 'result.json'
		result = json.loads(result_file.read_text()) if result_file.exists() else {}
		check(c, 'run succeeded', (record['state'], record['outcome']) == ('succeeded', 'succeeded'), f'{record["state"]}/{record["outcome"]} {record.get("detail")}')
		check(c, 'typed result.json', result.get('success') is True and result.get('result') == f'sent {who}' and result.get('run') == str(run), result)
		check(c, 'fixture saw one submission', records()[len(before):].count('name=' + who.replace(' ', '%20')) == 1, records()[len(before):])
		check(c, 'record names image and browser', bool(record['image_digest']) and record['browser_version'].startswith('Helium/'), f'{record["image"]} {record["browser_version"]}')
		if client == 'agent-browser':
			shot = Path(record['output_dir']) / 'form.png'
			check(c, 'screenshot in the output', shot.exists() and shot.read_bytes()[:8] == PNG)
		check(c, 'no capability URL in log or files', not leaks(run, record), leaks(run, record))
		check(c, 'session still ready', session_ready())
		check(c, 'controller released', controller_free() and client_sockets() == 0)
		marker = os.environ['AGENT_BROWSER'] if client == 'agent-browser' else venv
		check(c, 'no client process left', not leftovers(marker), leftovers(marker))

	for job, client, who in (('bu-hold', 'browser-use', 'Ada hold'), ('ab-hold', 'agent-browser', 'Lin hold')):
		c = f'job/{client}'[:13]
		key = 'name=' + who.replace(' ', '%20')
		run = start_job(config, job)
		deadline = time.monotonic() + 120
		while key not in records() and time.monotonic() < deadline:
			time.sleep(0.2)
		bctl('cancel', str(run))
		record = wait_run(run, ended, timeout=60)
		time.sleep(3)
		check(c, 'cancel ends the run once', (record['outcome'], len(record['attempts'])) == ('cancelled', 1), f'{record["outcome"]} attempts={len(record["attempts"])}')
		check(c, 'side effect not repeated', records().count(key) == 1, records().count(key))
		marker = os.environ['AGENT_BROWSER'] if client == 'agent-browser' else venv
		check(c, 'no client process left', not leftovers(marker), leftovers(marker))
		check(c, 'controller released', controller_free() and client_sockets() == 0)

	key = 'name=Ada%20kill'
	run = start_job(config, 'bu-kill')
	deadline = time.monotonic() + 120
	while key not in records() and time.monotonic() < deadline:
		time.sleep(0.2)
	daemon = subprocess.run(['lsof', '-t', str(Path(os.environ['BCTL_HOME']) / 'daemon.lock')], capture_output=True, text=True).stdout.strip()
	subprocess.run(['kill', '-9', daemon])
	record = wait_run(run, ended, timeout=60)
	time.sleep(3)
	check('job/daemon', 'kill -9 interrupts, no retry', (record['outcome'], len(record['attempts'])) == ('interrupted', 1), f'{record["outcome"]} attempts={len(record["attempts"])}')
	check('job/daemon', 'side effect not repeated', records().count(key) == 1, records().count(key))
	check('job/daemon', 'session still ready', session_ready())
	time.sleep(17)
	check('job/daemon', 'no client process left', not leftovers(venv), leftovers(venv))
	check('job/daemon', 'controller released', controller_free() and client_sockets() == 0)

	with tempfile.TemporaryDirectory(dir=os.environ['TMPDIR']) as isolated:
		for client, program in (('browser-use', 'browser_use_form.py'), ('agent-browser', 'agent_browser_form.py')):
			c = f'job/{client}'[:13]
			out = Path(isolated) / client
			out.mkdir()
			env = {**os.environ, 'TMPDIR': isolated, 'BCTL_CDP_URL': f'http://{secrets.token_hex(24)}.localhost:{urllib.parse.urlsplit(cdp_url).port}',
				   'BCTL_OUTPUT_DIR': str(out), 'BCTL_RUN': '0'}  # fmt: skip
			done = subprocess.run([sys.executable, str(HERE / 'jobs' / program), SITE + '/form', 'Nobody', '0'], capture_output=True, text=True, env=env, timeout=120)
			result = json.loads((out / 'result.json').read_text()) if (out / 'result.json').exists() else {}
			made = [p.name for p in Path(isolated).iterdir() if p.name not in ('browser-use', 'agent-browser')]
			check(c, 'bad URL fails the job', done.returncode != 0 and result.get('success') is False, f'exit {done.returncode} {result.get("errors")}')
			check(c, 'bad URL starts no browser', not made and not leftovers(isolated), made or leftovers(isolated))
			check(c, 'bad URL redacted in output', not TOKEN.search(done.stdout + done.stderr + json.dumps(result)))


def session_view(session: str = SESSION) -> dict:
	return (json.loads(bctl('inspect', session, '--json').stdout or '{}').get('sessions') or [{}])[0]


def guest_text(session: str, url: str) -> str:
	"""Opens `url` in a new tab of `session` and returns its body text, as JSON."""
	bctl('tab', 'new', session, url)
	bctl('page', 'wait', session, '--load', 'load')
	return bctl('page', 'eval', session, 'document.body.innerText').stdout.strip()


def finished_download(name: str) -> bool:
	return not name.endswith('.crdownload') and not name.startswith('.')


def arrived(directory: Path, before: set[str], timeout: float = 30) -> list[Path]:
	"""Waits for new finished files in `directory`; returns them."""
	deadline = time.monotonic() + timeout
	while time.monotonic() < deadline:
		new = sorted(p for p in directory.iterdir() if p.name not in before and finished_download(p.name))
		if new:
			return new
		time.sleep(0.5)
	return []


async def file_checks(scratch: Path) -> None:
	import hashlib

	view = session_view()
	runtime = os.environ['BCTL_RUNTIME']
	fields = {k: view.get(k) for k in ('display', 'input', 'pausable', 'guest_files_dir')}
	expected = {'smolvm': {'display': 'headed', 'pausable': True, 'guest_files_dir': '/home/browser/Downloads'},
				'nanos': {'display': 'headless', 'input': 'headless', 'pausable': False, 'guest_files_dir': '/home/browser/Downloads'}}[runtime]  # fmt: skip
	check('capabilities', 'inspect names what the runtime does', all(fields[k] == v for k, v in expected.items()), fields)
	shared, guest = Path(view['files_dir']), view['guest_files_dir']

	blob = urllib.request.urlopen(f'http://127.0.0.1:{FIXTURE_PORT}/blob.bin', timeout=10).read()
	payload = shared / 'payload.bin'
	payload.write_bytes(os.urandom(150_000))
	sent = f'upload={hashlib.sha256(payload.read_bytes()).hexdigest()}&bytes={payload.stat().st_size}'
	note = shared / 'note.txt'
	note.write_text('from the host')
	check('files', 'guest reads a host file', guest_text(SESSION, f'file://{guest}/note.txt') == '"from the host"')

	c = 'browser-use'
	host_payload = scratch / 'payload.bin'
	host_payload.write_bytes(payload.read_bytes())
	before = {p.name for p in shared.iterdir()}
	cells = [
		'R = {}',
		guarded(
			f"new_tab({SITE + '/upload'!r}); wait_for_load()\nR['set'] = upload_file('#f', {str(host_payload)!r})\n{click('#send')}\n"
			"for _ in range(100):\n\tif js(\"document.querySelector('#out').textContent\"): break\n\twait(0.1)\n"
			"R['out'] = js(\"document.querySelector('#out').textContent\")"
		),
		guarded(
			f"goto_url({SITE + '/download'!r}); wait_for_load()\n"
			f"cdp('Browser.setDownloadBehavior', behavior='allow', downloadPath={guest!r})\n"
			f"{click('#dl')}\nwait(1)"
		),
	]
	value, _ = await agent_run(cells, 'json.dumps(R)')
	r = json.loads(value or '{}')
	check(c, 'upload_file of a host path', sent in records() and r.get('out') == 'uploaded', f'{r} {[x for x in records() if x.startswith("upload=")]}')
	new = arrived(shared, before)
	check(c, 'download lands in files_dir', [p.read_bytes() == blob for p in new] == [True], [p.name for p in new])

	c, s = 'agent-browser', f'interop{os.getpid()}files'
	before_records = len(records())
	log = steps(s, ('tab', 'new', SITE + '/upload'), ('upload', '#f', f'{guest}/payload.bin'), ('click', '#send'), ('wait', '--text', 'uploaded'))
	check(c, 'upload of a guest_files_dir path', sent in records()[before_records:], f'{records()[before_records:]} after {log}')
	agent_browser(s, 'close')

	other = f'{SESSION}-other'
	up = bctl('up', other, '--profile', f'{SESSION}-other', '--image', os.environ['BCTL_IMAGE'], '--allow-private', '100.96.0.0/30')
	if check('profiles', 'second session on another profile', up.returncode == 0, up.stderr.strip()[-200:]):
		bctl('tab', 'new', SESSION, SITE + '/form')
		bctl('page', 'eval', SESSION, "document.cookie = 'e4=first; max-age=3600'")
		bctl('tab', 'new', other, SITE + '/form')
		bctl('page', 'wait', other, '--load', 'load')
		cookie = bctl('page', 'eval', other, 'document.cookie').stdout.strip()
		check('profiles', 'cookies stay in their profile', cookie == '""', cookie)
		other_shared = Path(session_view(other).get('files_dir') or scratch / 'missing')
		listing = guest_text(other, f'file://{guest}/')
		check('files', 'files stay in their session', other_shared != shared and 'note.txt' not in listing, f'{other_shared} {listing[:200]}')
		bctl('down', other)
		bctl('rm', other)
		check('files', 'rm removes files_dir', not other_shared.exists(), other_shared)


def plugin(*args: str, request: dict | None = None, kind: str = 'browser.launch') -> dict:
	"""Sends one agent-browser.plugin.v1 request to `bctl agent-browser-plugin ARGS`; returns its response."""
	envelope = {'protocol': 'agent-browser.plugin.v1', 'type': kind, 'capability': 'plugin.manifest' if kind == 'plugin.manifest' else 'browser.provider',
				'request': request or {}}  # fmt: skip
	done = subprocess.run([os.environ['BCTL'], 'agent-browser-plugin', *args], input=json.dumps(envelope), capture_output=True, text=True, timeout=120)
	return json.loads(done.stdout or '{}')


def provider(session: str, *args: str, plugin_args: tuple[str, ...]) -> subprocess.CompletedProcess:
	"""Runs agent-browser with bctl as its `browser.provider` plugin, configured explicitly and not over CDP."""
	config = [{'name': 'bctl', 'command': os.environ['BCTL'], 'args': ['agent-browser-plugin', *plugin_args], 'capabilities': ['browser.provider']}]
	environment = {k: v for k, v in os.environ.items() if k != 'AGENT_BROWSER_CDP'}
	environment |= {'AGENT_BROWSER_SESSION': session, 'AGENT_BROWSER_PLUGINS': json.dumps(config), 'AGENT_BROWSER_NO_AUTO_DIALOG': '1'}
	return subprocess.run([os.environ['AGENT_BROWSER'], *args], capture_output=True, text=True, timeout=120, env=environment)


def gateway_answers(url: str) -> bool:
	try:
		return urllib.request.urlopen(f'{url}/json/version', timeout=10).status == 200
	except (urllib.error.URLError, OSError):
		return False


def plugin_checks() -> None:
	c = 'ab-provider'
	owned = ('--image', os.environ['BCTL_IMAGE'], '--allow-private', '100.96.0.0/30')
	home = Path(os.environ['BCTL_HOME'])
	manifest = plugin(kind='plugin.manifest').get('manifest', {})
	check(c, 'manifest', manifest.get('name') == 'bctl' and manifest.get('capabilities') == ['browser.provider'], manifest)

	s = f'p{os.getpid()}'
	opened = provider(s, '--provider', 'bctl', 'open', SITE + '/form', plugin_args=owned)
	title = out(provider(s, 'get', 'title', plugin_args=owned))
	view = session_view(f'ab-{s}')
	check(c, 'launch starts an owned session', opened.returncode == 0 and title == 'form' and view.get('state') == 'ready', f'{out(opened)} {title} {view.get("state")}')
	closed = provider(s, 'close', plugin_args=owned)
	gone = 'NO_SUCH_SESSION' in bctl('inspect', f'ab-{s}', '--json').stdout
	check(c, 'close removes the owned session', closed.returncode == 0 and gone and not (home / 'agent-browser' / f'ab-{s}.json').exists(), out(closed))
	again = plugin(request={'lease': 'owned', 'session': f'ab-{s}', 'url': 'http://x.localhost:1', 'generation': 1}, kind='browser.close')
	check(c, 'repeated close is a no-op', again.get('success') is True, again)

	s = f'p{os.getpid()}dead'
	provider(s, '--provider', 'bctl', 'open', 'about:blank', plugin_args=owned)
	first = (home / 'agent-browser' / f'ab-{s}.json').read_text() if (home / 'agent-browser' / f'ab-{s}.json').exists() else ''
	socket_dir = Path(os.environ.get('AGENT_BROWSER_SOCKET_DIR') or Path.home() / '.agent-browser')
	pid = int((socket_dir / f'{s}.pid').read_text().strip())
	os.kill(pid, 9)
	time.sleep(1)
	relaunched = provider(s, '--provider', 'bctl', 'open', SITE + '/form', plugin_args=owned)
	second = (home / 'agent-browser' / f'ab-{s}.json').read_text() if (home / 'agent-browser' / f'ab-{s}.json').exists() else ''
	first_url = json.loads(first or '{}').get('url', '')
	check(c, 'owner death: relaunch reclaims', relaunched.returncode == 0 and first and second and first != second and not gateway_answers(first_url),
		  f'{out(relaunched)}')  # fmt: skip
	provider(s, 'close', plugin_args=owned)
	check(c, 'reclaimed session closes', 'NO_SUCH_SESSION' in bctl('inspect', f'ab-{s}', '--json').stdout)

	s = f'p{os.getpid()}bad'
	failed = provider(s, '--provider', 'bctl', 'open', 'about:blank', plugin_args=('--image', 'no-such-image'))
	left = bctl('inspect', f'ab-{s}', '--json').stdout
	provider(s, 'close', plugin_args=('--image', 'no-such-image'))
	check(c, 'failed launch leaves nothing', failed.returncode != 0 and 'NO_SUCH_SESSION' in left and not (home / 'agent-browser' / f'ab-{s}.json').exists(),
		  out(failed))  # fmt: skip

	lent = plugin('--session', SESSION, request={'provider': 'bctl', 'session': 'x', 'launchOptions': {'headed': False, 'engine': 'chrome'}})
	browser = lent.get('browser', {})
	url = browser.get('cdpUrl', '')
	check(c, 'borrowed lease answers CDP', gateway_answers(url) and browser.get('metadata', {}).get('lease') == 'borrowed', browser.get('metadata'))
	closed = plugin('--session', SESSION, request=browser.get('cleanup', {}), kind='browser.close')
	check(c, 'borrowed close revokes only its URL', closed.get('success') is True and not gateway_answers(url) and gateway_answers(cdp_url)
		  and session_view().get('state') == 'ready', closed)  # fmt: skip

	refused = plugin(*owned, request={'session': f'p{os.getpid()}x', 'launchOptions': {'engine': 'lightpanda'}})
	check(c, 'other engines refused', refused.get('success') is False and 'lightpanda' in refused.get('error', ''), refused)
	headed = plugin(*owned, request={'session': f'p{os.getpid()}h', 'launchOptions': {'headed': True}})
	expected = os.environ['BCTL_RUNTIME'] == 'smolvm'
	check(c, 'headed only where the runtime is', headed.get('success') is expected, headed.get('error') or headed.get('browser', {}).get('metadata'))
	if headed.get('success'):
		plugin(request=headed['browser']['cleanup'], kind='browser.close')
	check(c, 'no plugin session left', not [v['name'] for v in json.loads(bctl('status', '--json').stdout).get('sessions', []) if v['name'].startswith('ab-')])
	prefix = f'bctl-ab-p{os.getpid()}'
	if os.environ['BCTL_RUNTIME'] == 'smolvm':
		listed = subprocess.run([str(home / 'runtime/smolvm/smolvm'), 'machine', 'ls', '--json'], capture_output=True, text=True).stdout
		machines = [m['name'] for m in json.loads(listed or '[]') if m['name'].startswith(prefix)]
	else:
		machines = subprocess.run(['pgrep', '-f', f'machines/{prefix}'], capture_output=True, text=True).stdout.split()
	check(c, 'no plugin machine left running', not machines, machines)


async def revocation_checks() -> None:
	held = f'interop{os.getpid()}held'
	agent_browser(held, 'open', SITE + '/form')
	revoked = bctl('cdp-revoke', SESSION)
	check('gateway', 'cdp-revoke succeeds', revoked.returncode == 0, revoked.stderr.strip())
	status, _ = get(f'{cdp_url}/json/version')
	check('gateway', 'revoked URL is refused', status != 200, status)
	time.sleep(1)
	check('gateway', 'revoke closes open sockets', client_sockets() == 0, client_sockets())
	after = agent_browser(held, 'eval', 'document.title')
	check('agent-browser', 'revoke ends an open connection', after.returncode != 0, out(after))
	agent_browser(held, 'close')
	fresh = agent_browser(f'{held}2', 'open', SITE + '/form')
	check('agent-browser', 'revoked URL cannot attach', fresh.returncode != 0, out(fresh))
	agent_browser(f'{held}2', 'close')
	try:
		await Browser(cdp_url=cdp_url).start()
		check('browser-use', 'revoked URL cannot attach', False, 'attached')
	except Exception as error:
		check('browser-use', 'revoked URL cannot attach', 'Unauthorized' in str(error), type(error).__name__)
	check('gateway', 'no client socket left open', client_sockets() == 0, client_sockets())


async def main() -> None:
	scratch = Path(tempfile.mkdtemp(prefix='interop-'))
	helium = gateway_checks()
	await browser_use_checks(scratch)
	agent_browser_checks(scratch)
	await file_checks(scratch)
	plugin_checks()
	await revocation_checks()
	job_checks(scratch)
	print(json.dumps({'helium': helium}))


asyncio.run(main())
sys.exit(1 if failures else 0)
