#!/usr/bin/env python3
"""Official Python + JS SDK compatibility, isolated local origin and persistent store."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import urllib.request
from firecrawl import Firecrawl, __version__

ROOT = Path(__file__).resolve().parents[2]


class Origin(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/robots.txt': body = 'User-agent: *\nDisallow: /blocked\n'
        else:
            if self.path == '/slow': time.sleep(2)
            body = '<article><h1>SDK_TEST_ARTICLE</h1><p>' + ('A complete static text for SDK validation. ' * 60) + '</p>'
            body += '<a href="/article">Article</a><a href="/other">Other</a></article>'
        self.send_response(200); self.send_header('Content-Type', 'text/plain' if self.path == '/robots.txt' else 'text/html; charset=utf-8'); self.end_headers()
        try: self.wfile.write(body.encode())
        except BrokenPipeError: pass

    def log_message(self, *_): pass


def main():
    out = Path(sys.argv[1]).resolve(); out.mkdir(parents=True, exist_ok=False)
    binary = sys.argv[2]
    origin = ThreadingHTTPServer(('127.0.0.1', 0), Origin)
    threading.Thread(target=origin.serve_forever, daemon=True).start()
    with socket.socket() as probe: probe.bind(('127.0.0.1', 0)); port = probe.getsockname()[1]
    api = f'http://127.0.0.1:{port}'; base = f'http://127.0.0.1:{origin.server_port}/'
    config = out / 'pagesift.toml'
    config.write_text('[fetch]\nallow_private_network = true\n[policy]\ninterval_ms = 0\n'
                      '[cache]\nenabled = false\ndir = ' + json.dumps(str(out / 'store')) + '\n')
    checks = []
    def check(name, value): checks.append({'name': name, 'ok': bool(value)})
    with (out / 'service.log').open('w') as log:
        service = subprocess.Popen([binary, 'serve', '--port', str(port), '--api-key', 'test-key', '--config', str(config)], stdout=log, stderr=log)
        try:
            for _ in range(100):
                try:
                    with urllib.request.urlopen(api + '/healthz', timeout=.5): break
                except OSError:
                    if service.poll() is not None: raise RuntimeError('service exited')
                    time.sleep(.1)
            else: raise RuntimeError('service readiness')
            client = Firecrawl(api_key='test-key', api_url=api)
            try:
                doc = client.scrape(base, formats=['markdown', 'links'])
                check('scrape-markdown', 'SDK_TEST_ARTICLE' in (doc.markdown or ''))
                check('scrape-metadata', doc.metadata.status_code == 200)
                check('scrape-links', len(doc.links or []) >= 2)
                mapped = client.map(base, sitemap='skip', limit=10)
                check('map', len(mapped.links) >= 2)
                batch = client.batch_scrape([base, base + 'article'], formats=['markdown'], poll_interval=1, wait_timeout=30)
                check('batch-polling', batch.status == 'completed' and len(batch.data) == 2)
                crawled = client.crawl(base, sitemap='skip', limit=3, max_discovery_depth=1, poll_interval=1, timeout=30)
                check('crawl-polling', crawled.status == 'completed' and len(crawled.data) >= 2)
                job = client.start_crawl(base + 'slow', limit=1)
                check('cancel', client.cancel_crawl(job.id))
                for name, f, expected in [
                    ('unauthorized', lambda: Firecrawl(api_key='wrong', api_url=api).scrape(base), 'UnauthorizedError'),
                    ('robots', lambda: client.scrape(base + 'blocked'), 'WebsiteNotSupportedError')]:
                    try: f(); check(name, False)
                    except Exception as e: checks.append({'name': name, 'ok': type(e).__name__ == expected, 'exception': type(e).__name__})
            except Exception as e:
                checks.append({'name': 'unexpected-error', 'ok': False, 'error': repr(e)})
            (out / 'python.json').write_text(json.dumps({'sdk_version': __version__, 'checks': checks}, indent=2) + '\n')
            with (out / 'js.log').open('w') as jslog:
                js = subprocess.run(['node', 'bench/six/adapters/sdk.mjs', api, base, str(out / 'js.json')],
                                    stdout=jslog, stderr=jslog, timeout=90)
        finally:
            service.terminate()
            try: service.wait(timeout=15)
            except subprocess.TimeoutExpired: service.kill(); service.wait()
            origin.shutdown(); origin.server_close()
    print(json.dumps({'python': checks, 'js_exit': js.returncode}))
    if any(not r['ok'] for r in checks) or js.returncode: raise SystemExit(1)


if __name__ == '__main__': main()
