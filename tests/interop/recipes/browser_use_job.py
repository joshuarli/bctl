"""A bctl exec job that runs a browser-use 1.0.0 Agent in the session's browser.

    [jobs.research]
    kind = "exec"
    browser = "default"
    argv = ["python3", "browser_use_job.py", "--model", "anthropic:claude-sonnet-5-5", "Find the opening hours"]

The Agent attaches to Helium through BCTL_CDP_URL only: never Browser() (a Browser Use Cloud browser) and never a
local Chromium. The model must be named: browser-use's default is its paid gateway. Writes result.json to
BCTL_OUTPUT_DIR and exits 0 only if the Agent finished successfully. Stopping the Agent's client leaves the session
running; `bctl down` stops it. The capability URL is redacted from everything this program prints or writes.
"""

import argparse
import asyncio
import json
import logging
import os
import sys
from pathlib import Path
from urllib.parse import urlsplit

os.environ.setdefault('ANONYMIZED_TELEMETRY', 'false')

from browser_use import Agent, Browser  # noqa: E402

CDP_URL = os.environ['BCTL_CDP_URL']
TOKEN_HOST = urlsplit(CDP_URL).hostname or CDP_URL


def redact(text: str) -> str:
	return text.replace(CDP_URL, '<cdp-url>').replace(TOKEN_HOST, '<token>.localhost')


class Redacting(logging.Filter):
	def filter(self, record: logging.LogRecord) -> bool:
		record.msg, record.args = redact(record.getMessage()), ()
		return True


def redact_logs() -> None:
	for handler in logging.getLogger().handlers + [h for name in logging.root.manager.loggerDict for h in getattr(logging.getLogger(name), 'handlers', [])]:
		handler.addFilter(Redacting())


async def run(task: str, model: object) -> int:
	"""Runs the Agent with `model` (a model string or a Pydantic AI model) and writes the run's result.json."""
	redact_logs()
	output = Path(os.environ['BCTL_OUTPUT_DIR'])
	browser = Browser(cdp_url=CDP_URL)
	result, failure = None, None
	try:
		history = await Agent(task, model, browser=browser, workspace=output / 'workspace').run()
		result = history.final_result()
		success = bool(history.is_successful())
		errors = [e for e in history.errors() if e]
	except Exception as error:
		success, failure, errors = False, f'{type(error).__name__}: {error}', []
	finally:
		await browser.stop()
	report = {'run': os.environ.get('BCTL_RUN'), 'success': success, 'result': result, 'errors': errors + ([failure] if failure else [])}
	text = redact(json.dumps(report))
	(output / 'result.json').write_text(text + '\n')
	print(text, flush=True)
	return 0 if success else 1


def main() -> int:
	parser = argparse.ArgumentParser(description='Run a browser-use Agent in a bctl session.')
	parser.add_argument('--model', required=True, help='a Pydantic AI model string, e.g. anthropic:claude-sonnet-5-5')
	parser.add_argument('task')
	args = parser.parse_args()
	return asyncio.run(run(args.task, args.model))


if __name__ == '__main__':
	sys.exit(main())
