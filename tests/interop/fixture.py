"""Fixture site for tests/interop: pages the guest loads from the host, and the requests they made.

The guest reaches the host at 100.96.0.1. The cross-origin iframe uses the IPv4-mapped IPv6 form of the same address,
which Chromium treats as another site, so the frame gets its own process and CDP target. GET /_records returns every
/record query string the pages sent, as JSON; a POST to /upload records its body's SHA-256 the same way.
/blob.bin is an attachment of random bytes, fixed for the server's lifetime. Prints the port on its first line of
output.
"""

import hashlib
import http.server
import json
import os
import sys
import threading
from urllib.parse import urlsplit

HOST = '100.96.0.1'
OTHER_SITE = '[::ffff:100.96.0.1]'
records: list[str] = []
BLOB = os.urandom(200_000)
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
	if path == '/upload':
		return (
			'<!doctype html><title>upload</title><input type=file id=f>'
			"<button id=send onclick=\"fetch('/upload', {method: 'POST', body: document.getElementById('f').files[0]})"
			".then(r => out.textContent = r.ok ? 'uploaded' : 'failed')\">Send</button><p id=out></p>"
		)
	if path == '/download':
		return '<!doctype html><title>download</title><a id=dl href=/blob.bin>Get the file</a>'
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
		elif url.path == '/blob.bin':
			self.reply(200, 'application/octet-stream', BLOB, {'Content-Disposition': 'attachment; filename="blob.bin"'})
		elif (html := page(url.path, url.query, self.server.server_port)) is not None:
			self.reply(200, 'text/html', html.encode())
		else:
			self.reply(404, 'text/plain', b'not found')

	def do_POST(self) -> None:
		if urlsplit(self.path).path != '/upload':
			return self.reply(404, 'text/plain', b'not found')
		body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
		with lock:
			records.append(f'upload={hashlib.sha256(body).hexdigest()}&bytes={len(body)}')
		self.reply(204, 'text/plain', b'')

	def reply(self, status: int, content_type: str, body: bytes, headers: dict[str, str] | None = None) -> None:
		self.send_response(status)
		self.send_header('Content-Type', content_type)
		for name, value in (headers or {}).items():
			self.send_header(name, value)
		self.send_header('Content-Length', str(len(body)))
		self.send_header('Cache-Control', 'no-store')
		self.end_headers()
		self.wfile.write(body)

	def log_message(self, *args) -> None:
		pass


class Server(http.server.ThreadingHTTPServer):
	request_queue_size = 128


server = Server(('127.0.0.1', 0), Handler)
print(server.server_port, flush=True)
sys.stdout.close()
server.serve_forever()
