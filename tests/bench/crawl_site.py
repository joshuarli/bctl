"""Deterministic linked pages with dynamic CSS, fetched content, and lazy images."""

import json

CSS = b'body{font:16px sans-serif;max-width:760px;margin:24px auto}img{width:320px;height:160px}section{padding:16px;background:#def}'
DYNAMIC_CSS = b'#dynamic{border:3px solid #357;color:#123}nav{display:flex;gap:12px}'
IMAGE = b'<svg xmlns="http://www.w3.org/2000/svg" width="320" height="160"><rect width="320" height="160" fill="#357"/><text x="20" y="80" fill="white">Saved crawl asset</text></svg>'


def write(root, pages):
    root.mkdir(parents=True)
    (root / 'assets').mkdir()
    (root / 'data').mkdir()
    for name, content in [('base.css', CSS), ('dynamic.css', DYNAMIC_CSS), ('image.svg', IMAGE), ('lazy.svg', IMAGE)]:
        (root / 'assets' / name).write_bytes(content)
    for index in range(pages):
        links = ''.join(f'<a href="/page-{target}.html">Page {target}</a>' for target in range(pages))
        paragraphs = ''.join(f'<p>Page {index}, section {section}: deterministic crawl content with persisted assets and offline replay.</p>' for section in range(24))
        (root / 'data' / f'{index}.json').write_text(json.dumps({'text': f'dynamic page {index} complete'}))
        (root / f'page-{index}.html').write_text(f'''<!doctype html><meta charset="utf-8"><title>Crawl page {index}</title>
<link rel="stylesheet" href="/assets/base.css"><nav>{links}</nav><h1>Page {index}</h1>
<img src="/assets/image.svg" alt="Saved crawl asset"><section id="dynamic">Loading dynamic content</section>
{paragraphs}<img loading="lazy" src="/assets/lazy.svg" alt="Saved lazy asset">
<script>
const style = document.createElement('link'); style.rel = 'stylesheet'; style.href = '/assets/dynamic.css'; document.head.append(style);
fetch('/data/{index}.json').then(r => r.json()).then(data => document.querySelector('#dynamic').textContent = data.text);
</script>''')
