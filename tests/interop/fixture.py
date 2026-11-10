"""Fixture site for tests/interop: pages the guest loads from the host, and the requests they made.

The guest reaches the host at 100.96.0.1. The cross-origin iframe uses the IPv4-mapped IPv6 form of the same address,
which Chromium treats as another site, so the frame gets its own process and CDP target. GET /_records returns every
/record query string the pages sent, as JSON. Prints the port on its first line of output.
"""

import http.server
import json
import sys
import threading
from urllib.parse import urlsplit

HOST = '100.96.0.1'
OTHER_SITE = '[::ffff:100.96.0.1]'
records: list[str] = []
lock = threading.Lock()


def page(path: str, query: str, port: int) -> str | None:
	if path == '/form':
		return (
			'<!doctype html><title>form</title><label for=name>Name</label><input id=name>'
			"<button id=submit onclick=\"const v = document.getElementById('name').value; "
			"fetch('/record?name=' + encodeURIComponent(v)).then(() => out.textContent = 'sent ' + v)\">Submit</button>"
			'<p id=out></p>'
		)
	if path == '/dialog':
		return (
			'<!doctype html><title>dialog</title>'
			"<button id=ask onclick=\"document.title = confirm('sure?') ? 'confirmed' : 'dismissed'\">Ask</button>"
		)
	if path == '/popup':
		return "<!doctype html><title>opener</title><button id=pop onclick=\"window.open('/popped', '_blank')\">Pop</button>"
	if path == '/popped':
		return '<!doctype html><title>popped</title><p>popped'
	if path == '/frames':
		return (
			'<!doctype html><title>frames</title><iframe id=same src="/inner?f=same"></iframe>'
			f'<iframe id=cross src="http://{OTHER_SITE}:{port}/inner?f=cross"></iframe>'
		)
	if path == '/inner':
		frame = 'cross' if query == 'f=cross' else 'same'
		return (
			f'<!doctype html><title>inner-{frame}</title>'
			f"<button id=b onclick=\"fetch('/record?frame={frame}'); this.textContent = 'clicked'\">Press</button>"
		)
	return None


class Handler(http.server.BaseHTTPRequestHandler):
	def do_GET(self) -> None:
		url = urlsplit(self.path)
		if url.path == '/record':
			with lock:
				records.append(url.query)
			self.reply(204, 'text/plain', b'')
		elif url.path == '/_records':
			with lock:
				body = json.dumps(records).encode()
			self.reply(200, 'application/json', body)
		elif (html := page(url.path, url.query, self.server.server_port)) is not None:
			self.reply(200, 'text/html', html.encode())
		else:
			self.reply(404, 'text/plain', b'not found')

	def reply(self, status: int, content_type: str, body: bytes) -> None:
		self.send_response(status)
		self.send_header('Content-Type', content_type)
		self.send_header('Content-Length', str(len(body)))
		self.send_header('Cache-Control', 'no-store')
		self.end_headers()
		self.wfile.write(body)

	def log_message(self, *args) -> None:
		pass


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
print(server.server_port, flush=True)
sys.stdout.close()
server.serve_forever()
