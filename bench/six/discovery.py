#!/usr/bin/env python3
"""Controlled same-origin discovery graph; content visits are scored from origin logs."""
import collections
import json
from pathlib import Path
import re
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / 'bench/data/six'
PARAGRAPH = 'This article explains a distinct useful subject for retrieval and indexing. ' * 30
GOLD = {'/', '/anchor', '/area', '/link-header', '/frame', '/htmx', '/script-route'}


class Origin(BaseHTTPRequestHandler):
    def do_GET(self):
        path = urlsplit(self.path).path
        self.server.requests.append({'path': path, 'method': 'GET', 't': time.monotonic()})
        if path == '/robots.txt':
            self.send_response(200); self.end_headers(); self.wfile.write(b'User-agent: *\nDisallow: /blocked\n'); return
        if path == '/sitemap.xml':
            self.send_response(404); self.end_headers(); return
        if path == '/':
            body = ('<a href="/anchor">Article</a><map><area href="/area" alt="Area"></map>'
                    '<iframe src="/frame"></iframe><div hx-get="/htmx">Read more</div>'
                    '<script>const article = "/script-route";</script>'
                    '<a href="/blocked">Forbidden</a><img src="/asset.png">')
        elif path in GOLD:
            body = f'<p>{PARAGRAPH}</p><a href="/">Home</a>'
        elif path == '/blocked':
            body = '<p>Should not be requested when robots is respected.</p>'
        else:
            self.send_response(404); self.end_headers(); return
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        if path == '/':
            self.send_header('Link', '</link-header>; rel="next"')
        self.end_headers()
        self.wfile.write(f'<html><body><article><h1>{path}</h1>{body}</article></body></html>'.encode())

    def log_message(self, *_):
        pass


def main():
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    server = ThreadingHTTPServer(('127.0.0.1', 0), Origin)
    server.requests = []
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f'http://127.0.0.1:{server.server_port}/'
    config = out / 'pagesift.toml'
    config.write_text('[fetch]\nallow_private_network = true\n[policy]\ninterval_ms = 0\nper_host_concurrency = 2\nglobal_concurrency = 2\n[cache]\nenabled = false\n')
    tasks = [
        ('pagesift', ['target/release/pagesift', 'crawl', url, '--config', str(config),
                      '--engine', 'http', '--sitemap', 'skip', '--depth', '3', '--limit', '50']),
        ('crawlee', ['node', 'bench/six/adapters/crawlee.mjs', url, str(out / 'crawlee-pages.json')]),
        ('katana', [str(DATA / 'bin/katana'), '-u', url, '-d', '3', '-jc', '-c', '2', '-p', '1',
                    '-mdp', '50', '-ct', '20s', '-timeout', '5', '-retry', '1', '-silent', '-nc',
                    '-jsonl', '-duc', '-fs', 'fqdn']),
    ]
    # A separate budget/scope track, not a claim of native robots support. The
    # controlled fixture's one Disallow path is explicitly excluded in this run.
    tasks.append(('katana-explicit-scope', tasks[-1][1] +
                  ['-cos', '/blocked(?:$|\\?)', '-H', 'User-Agent: pagesift-comparison/1.0']))
    if len(sys.argv) > 2:
        tasks[0] = ('pagesift-baseline', [sys.argv[2], *tasks[0][1][1:]])
    if len(sys.argv) > 3:
        tasks.insert(1, ('pagesift-candidate', [sys.argv[3], *tasks[0][1][1:]]))
    summary = {}
    try:
        for name, command in tasks:
            server.requests = []
            env = dict(__import__('os').environ)
            env['CRAWLEE_STORAGE_DIR'] = str(DATA / 'crawlee-discovery-storage')
            env['CRAWLEE_PURGE_ON_START'] = 'true'
            started = time.monotonic()
            with (out / f'{name}.stdout').open('w') as stdout, (out / f'{name}.stderr').open('w') as stderr:
                proc = subprocess.run(command, cwd=ROOT, stdout=stdout, stderr=stderr, timeout=60, env=env)
            visits = list(server.requests)
            (out / f'{name}-origin.json').write_text(json.dumps(visits, indent=2) + '\n')
            seen = {r['path'] for r in visits}
            summary[name] = {'command': command, 'exit': proc.returncode,
                             'scope_mode': 'explicit fixture Disallow exclusion; not native robots support' if name == 'katana-explicit-scope' else 'native/default handler',
                             'wall_s': time.monotonic() - started, 'gold_content': sorted(GOLD),
                             'visited_content': sorted(seen & GOLD), 'missed_content': sorted(GOLD - seen),
                             'robots_violations': sum(r['path'] == '/blocked' for r in visits),
                             'requests': len(visits), 'paths': dict(collections.Counter(r['path'] for r in visits))}
            (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
            print(name, json.dumps(summary[name]), flush=True)
            if proc.returncode:
                raise RuntimeError(f'{name} failed; inspect logs')
    finally:
        server.shutdown(); server.server_close()


if __name__ == '__main__':
    main()
