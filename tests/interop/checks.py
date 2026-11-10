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
	return subprocess.run([os.environ['BCTL'], *args], capture_output=True, text=True, timeout=120)


def session_ready() -> bool:
	listed = json.loads(bctl('inspect', SESSION, '--json').stdout)
	return listed['sessions'][0]['state'] == 'ready'


def client_sockets() -> int:
	"""Established host connections to the gateway port, counted at the client end."""
	port = urllib.parse.urlsplit(cdp_url).port
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
	await revocation_checks()
	print(json.dumps({'helium': helium}))


asyncio.run(main())
sys.exit(1 if failures else 0)
