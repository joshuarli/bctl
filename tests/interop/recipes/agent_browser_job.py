"""agent-browser 0.42.0 in a bctl exec job: a client session of the run's own, attached to the session's browser.

    from agent_browser_job import AgentBrowser

    with AgentBrowser('https://example.com/') as ab:
        ab('fill', '#q', 'opening hours')
        title = ab('get', 'title')

The client attaches only through BCTL_CDP_URL (AGENT_BROWSER_CDP, so the URL is not in any argv); it never launches
a browser of its own. Each run uses the client session bctl-run-BCTL_RUN, opens its own tab first (an attached
client otherwise drives whichever tab it finds, which may be a hidden one that drops input) and closes the client on
exit, including on the SIGTERM of `bctl cancel` or a timeout. agent-browser's daemon leaves the job's process
group, so closing it here is what ends it. Closing the client leaves the browser running. Command output is returned and printed with the capability URL redacted.
"""

import os
import signal
import subprocess
import sys
from urllib.parse import urlsplit

CDP_URL = os.environ['BCTL_CDP_URL']
TOKEN_HOST = urlsplit(CDP_URL).hostname or CDP_URL


def redact(text: str) -> str:
	return text.replace(CDP_URL, '<cdp-url>').replace(TOKEN_HOST, '<token>.localhost')


class Failed(Exception):
	pass


class AgentBrowser:
	def __init__(self, url: str, binary: str | None = None) -> None:
		self.url = url
		self.binary = binary or os.environ.get('AGENT_BROWSER', 'agent-browser')
		self.env = {**os.environ, 'AGENT_BROWSER_CDP': CDP_URL, 'AGENT_BROWSER_SESSION': f'bctl-run-{os.environ["BCTL_RUN"]}'}

	def __call__(self, *args: str) -> str:
		"""Runs one agent-browser command; returns its output, or raises Failed with its redacted error."""
		done = subprocess.run([self.binary, *args], capture_output=True, text=True, env=self.env, timeout=120)
		out, err = redact(done.stdout.strip()), redact(done.stderr.strip())
		print(f'agent-browser {redact(" ".join(args))}: {out or err}', flush=True)
		if done.returncode != 0:
			raise Failed(f'agent-browser {args[0]} exited {done.returncode}: {err or out}')
		return out

	def __enter__(self) -> 'AgentBrowser':
		signal.signal(signal.SIGTERM, lambda *_: sys.exit(128 + signal.SIGTERM))
		try:
			self('tab', 'new', self.url)
		except BaseException:
			self.__exit__()
			raise
		return self

	def __exit__(self, *exc: object) -> None:
		subprocess.run([self.binary, 'close'], capture_output=True, env=self.env, timeout=60)
