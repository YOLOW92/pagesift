#!/usr/bin/env python3
"""Same-origin HTTP API output regression; no Docker or timing claims."""
import hashlib
import json
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time
import urllib.request
from http.server import ThreadingHTTPServer
from firecrawl_full import Origin, DATA, request


def main():
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    rows = [json.loads(line) for dataset in ('aeb', 'fixtures')
            for line in (DATA / f'{dataset}.jsonl').read_text().splitlines()]
    for row in rows:
        assert hashlib.sha256(Path(row['path']).read_bytes()).hexdigest() == row['sha256']
    origin = ThreadingHTTPServer(('127.0.0.1', 0), Origin)
    origin.pages = {r['id']: r for r in rows}
    origin.requests = []
    threading.Thread(target=origin.serve_forever, daemon=True).start()
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0)); port = probe.getsockname()[1]
    api = f'http://127.0.0.1:{port}'
    outputs = {}
    try:
        for name, binary in zip(('baseline', 'candidate'), sys.argv[2:4], strict=True):
            config = out / f'{name}.toml'
            config.write_text('[fetch]\nallow_private_network = true\n[policy]\ninterval_ms = 0\n[cache]\nenabled = false\ndir = ' + json.dumps(str(out/name)) + '\n')
            with (out/f'{name}.log').open('w') as log:
                service = subprocess.Popen([binary, 'serve', '--port', str(port), '--config', str(config)], stdout=log, stderr=log)
                try:
                    for _ in range(100):
                        try:
                            with urllib.request.urlopen(api+'/healthz', timeout=.5): break
                        except OSError:
                            if service.poll() is not None: raise RuntimeError('API exited')
                            time.sleep(.1)
                    else: raise RuntimeError('API not ready')
                    outputs[name] = {}
                    with (out/f'{name}.jsonl').open('w') as f:
                        for row in rows:
                            status, response = request(api+'/v2/scrape', {'url': f'http://127.0.0.1:{origin.server_port}/case/{row["id"]}', 'formats': ['markdown','images','links'], 'engine':'http'})
                            assert status == 200 and response.get('success'), (row['id'], status, response)
                            doc=response['data']
                            assert doc['metadata']['statusCode'] == 200
                            record={'id':row['id'], 'markdown':doc.get('markdown'), 'images':doc.get('images'), 'links':doc.get('links')}
                            outputs[name][row['id']]=record
                            f.write(json.dumps(record)+'\n')
                finally:
                    service.terminate()
                    try: service.wait(timeout=10)
                    except subprocess.TimeoutExpired: service.kill(); service.wait()
        summary={'n':len(rows), 'markdown_identical':0,'images_identical':0,'links_changed':0,'binaries':{name:hashlib.sha256(Path(binary).read_bytes()).hexdigest() for name,binary in zip(('baseline','candidate'),sys.argv[2:4],strict=True)}}
        for row in rows:
            a,b=(outputs[n][row['id']] for n in ('baseline','candidate'))
            for key in ('markdown','images'): summary[key+'_identical'] += a[key]==b[key]
            summary['links_changed'] += a['links']!=b['links']
        (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
        print(json.dumps(summary))
        assert summary['markdown_identical']==summary['images_identical']==len(rows)
    finally:
        origin.shutdown();origin.server_close()


if __name__ == '__main__': main()
