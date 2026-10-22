#!/usr/bin/env python3
"""Reproduce host-level render learning errors, on an explicitly synthetic origin."""
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

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / 'bench/data/six'
ANSWER = 'DYNAMIC_ANSWER_4728'


class Origin(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/robots.txt':
            body = b'User-agent: *\nAllow: /\n'
            content_type = 'text/plain'
        else:
            content_type = 'text/html; charset=utf-8'
            if self.path.startswith('/static/'):
                article = '<h1>Short static article</h1><p>Complete text.</p>'
                script = ''
            else:
                article = '<h1>Loading</h1>'
                script = '<script>document.querySelector("article").innerHTML = ' + json.dumps(
                    '<h1>Dynamic article</h1><p>' + (ANSWER + ' ') * 40 + '</p>') + ';</script>'
            body = f'<html><body><article>{article}</article>{script}</body></html>'.encode()
        self.send_response(200); self.send_header('Content-Type', content_type); self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


def post(url, data):
    request = urllib.request.Request(url, data=json.dumps(data).encode(), headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.load(response)


def main():
    out = Path(sys.argv[1]).resolve(); out.mkdir(parents=True, exist_ok=False)
    binary = sys.argv[2] if len(sys.argv) > 2 else 'target/release/pagesift'
    origin = ThreadingHTTPServer(('127.0.0.1', 0), Origin)
    threading.Thread(target=origin.serve_forever, daemon=True).start()
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0)); port = probe.getsockname()[1]
    base = f'http://127.0.0.1:{origin.server_port}'
    urls = [base + f'/static/{i}' for i in range(4)] + [base + '/dynamic/product']
    config = out / 'pagesift.toml'
    config.write_text('[fetch]\nallow_private_network = true\n[policy]\ninterval_ms = 0\n'
                      '[browser]\npath = "/opt/google/chrome/google-chrome"\nmax_browsers = 1\nmax_tabs_per_browser = 1\n'
                      'min_available_memory_mb = 1024\n[cache]\nenabled = false\n'
                      f'dir = {json.dumps(str(out / "pagesift-store"))}\n')
    service_log = (out / 'pagesift-service.log').open('w')
    service = subprocess.Popen([binary, 'serve', '--port', str(port), '--config', str(config)],
                               stdout=service_log, stderr=service_log)
    results = []
    try:
        for _ in range(100):
            try:
                with urllib.request.urlopen(f'http://127.0.0.1:{port}/healthz', timeout=.5):
                    break
            except OSError:
                if service.poll() is not None: raise RuntimeError('pagesift service exited')
                time.sleep(.1)
        else: raise RuntimeError('pagesift readiness deadline')
        for url in urls:
            start = time.monotonic()
            doc = post(f'http://127.0.0.1:{port}/v2/scrape', {'url': url, 'engine': 'auto', 'formats': ['markdown'], 'timeout': 30000})
            results.append({'url': url, 'wall_s': time.monotonic() - start, 'response': doc})
        (out / 'pagesift.json').write_text(json.dumps(results, indent=2) + '\n')
    finally:
        service.terminate()
        try: service.wait(timeout=15)
        except subprocess.TimeoutExpired: service.kill(); service.wait()
        service_log.close()
    requests = out / 'urls.json'; requests.write_text(json.dumps(urls))
    env = dict(os.environ, CRAWLEE_STORAGE_DIR=str(out / 'crawlee-storage'), CRAWLEE_PURGE_ON_START='true')
    with (out / 'crawlee.log').open('w') as log:
        crawlee = subprocess.run(['node', 'bench/six/adapters/adaptive.mjs', str(requests), str(out / 'crawlee.json')],
                                 stdout=log, stderr=log, env=env, timeout=180)
    summary = {'synthetic': True, 'gold': ANSWER,
               'pagesift_dynamic_answer_found': ANSWER in results[-1]['response'].get('data', {}).get('markdown', ''),
               'pagesift_dynamic_response': results[-1]['response'], 'crawlee_exit': crawlee.returncode}
    if crawlee.returncode == 0:
        data = json.loads((out / 'crawlee.json').read_text())
        summary['crawlee_dynamic_answer_found'] = any(ANSWER in str(item) for item in data['items'])
    (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    origin.shutdown(); origin.server_close()
    print(json.dumps(summary))
    if crawlee.returncode: raise RuntimeError('Crawlee failed; inspect log')


if __name__ == '__main__':
    main()
