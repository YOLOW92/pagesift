#!/usr/bin/env python3
"""SIGKILL/restart against a real local origin. At-least-once requests are counted."""
import collections
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from urllib.parse import urlsplit
from firecrawl_full import request

ROOT=Path(__file__).resolve().parents[2]
N=int(os.environ.get('PAGESIFT_RECOVERY_N','80'))


class Origin(BaseHTTPRequestHandler):
    def do_GET(self):
        path=urlsplit(self.path).path
        self.server.requests.append({'path':path,'phase':self.server.phase})
        if path=='/robots.txt':body=b'User-agent: *\nAllow: /\n'
        elif path.startswith('/p/') and path[3:].isdigit() and int(path[3:])<N:
            time.sleep(.035)
            links=''.join(f'<a href="/p/{i}">Document {i}</a>' for i in range(1,N)) if path=='/p/0' else ''
            body=f'<article><h1>{path}</h1><p>Useful independent document. {"context "*80}</p>{links}</article>'.encode()
        else:self.send_response(404);self.end_headers();return
        self.send_response(200);self.send_header('Content-Type','text/html');self.end_headers()
        try:self.wfile.write(body)
        except (BrokenPipeError,ConnectionResetError):pass
    def log_message(self,*_):pass


def poll(predicate, timeout=90):
    end=time.monotonic()+timeout
    while time.monotonic()<end:
        value=predicate()
        if value:return value
        time.sleep(.03)
    raise TimeoutError('recovery wait deadline')


def main():
    out=Path(sys.argv[1]).resolve();out.mkdir(parents=True,exist_ok=False)
    origin=ThreadingHTTPServer(('127.0.0.1',0),Origin);origin.requests=[];origin.phase='first'
    threading.Thread(target=origin.serve_forever,daemon=True).start()
    base=f'http://127.0.0.1:{origin.server_port}'
    summary={}
    active=None
    try:
        for name,binary in zip(('pagesift-baseline','pagesift-candidate'),sys.argv[2:4],strict=True):
            run=out/name;run.mkdir();origin.requests=[];origin.phase='first'
            with socket.socket() as probe:probe.bind(('127.0.0.1',0));port=probe.getsockname()[1]
            api=f'http://127.0.0.1:{port}'
            cfg=run/'config.toml';cfg.write_text('[fetch]\nallow_private_network = true\n[policy]\ninterval_ms = 0\nglobal_concurrency = 2\nper_host_concurrency = 2\n[cache]\nenabled = false\ndir = '+json.dumps(str(run/'store'))+'\n')
            def start():
                log=(run/f'{origin.phase}.log').open('w')
                p=subprocess.Popen([binary,'serve','--port',str(port),'--config',str(cfg)],stdout=log,stderr=log);log.close()
                def ready():
                    if p.poll() is not None:raise RuntimeError('API exited')
                    try:
                        with urllib.request.urlopen(api+'/healthz',timeout=.5):return True
                    except OSError:return False
                poll(ready,15)
                return p
            active=start()
            status,job=request(api+'/v2/crawl',{'url':base+'/p/0','limit':N,'sitemap':'skip','crawlEntireDomain':True,'scrapeOptions':{'engine':'http','formats':['markdown']}})
            assert status==200 and job['success'],job
            path=api+'/v2/crawl/'+job['id']
            before=poll(lambda: (lambda r:r if r.get('completed',0)>=15 else None)(request(path)[1]))
            assert before['status']=='scraping',before
            saved={urlsplit(d['metadata']['sourceURL']).path for d in before['data']}
            active.kill();active.wait(timeout=10);active=None
            origin.phase='restart';active=start()
            final=poll(lambda:(lambda r:r if r.get('status')=='completed' else None)(request(path)[1]))
            delivered=[urlsplit(d['metadata']['sourceURL']).path for d in final['data']]
            requests=list(origin.requests)
            repeats=[r['path'] for r in requests if r['phase']=='restart' and r['path'] in saved]
            assert final['completed']==N and len(delivered)==N and len(set(delivered))==N
            assert set(delivered)=={f'/p/{i}' for i in range(N)} and not repeats
            active.terminate();active.wait(timeout=15);active=None
            origin.phase='reopen-completed';active=start()
            reopened=request(path)[1];assert reopened['status']=='completed' and reopened['completed']==N
            assert not any(r['phase']=='reopen-completed' and r['path'].startswith('/p/') for r in origin.requests)
            active.terminate();active.wait(timeout=15);active=None
            summary[name]={'n':N,'delivered':len(delivered),'unique_delivered':len(set(delivered)),
                'completed_before_kill':len(saved),'already_saved_urls_requested_after_restart':len(repeats),
                'content_requests':sum(r['path'].startswith('/p/') for r in requests),
                'completed_job_survives_second_restart':True,'signal':'SIGKILL'}
            (run/'origin.json').write_text(json.dumps(origin.requests,indent=2))
            (run/'final.json').write_text(json.dumps(final,indent=2))
            (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
        name='crawlee';run=out/name;run.mkdir();origin.requests=[];origin.phase='first'
        events=run/'events.jsonl';output=run/'dataset.json'
        env=dict(os.environ,CRAWLEE_STORAGE_DIR=str(run/'storage'),CRAWLEE_PURGE_ON_START='false')
        def start_crawlee():
            log=(run/f'{origin.phase}.log').open('w')
            p=subprocess.Popen(['node','bench/six/adapters/recovery.mjs',base+'/p/0',str(events),str(output)],env=env,stdout=log,stderr=log);log.close();return p
        active=start_crawlee()
        def interrupted():
            if active.poll() is not None:raise RuntimeError('Crawlee exited before interrupt')
            return events.exists() and len(events.read_text().splitlines())>=15
        poll(interrupted,30)
        saved={json.loads(l)['url'] for l in events.read_text().splitlines()}
        active.kill();active.wait(timeout=10);active=None
        origin.phase='restart';active=start_crawlee()
        code=active.wait(timeout=240);active=None
        if code:raise RuntimeError('Crawlee restart failed')
        data=json.loads(output.read_text())['items'];delivered=[r['url'] for r in data]
        assert {urlsplit(u).path for u in delivered}=={f'/p/{i}' for i in range(N)}
        summary[name]={'n':N,'delivered':len(delivered),'unique_delivered':len(set(delivered)),
            'handler_events_before_kill':len(saved),'already_saved_urls_requested_after_restart':sum(r['phase']=='restart' and base+r['path'] in saved for r in origin.requests),
            'content_requests':sum(r['path'].startswith('/p/') for r in origin.requests),'signal':'SIGKILL',
            'scope':'native local RequestQueue and Dataset; no custom dedup layer; no speed claim'}
        (run/'origin.json').write_text(json.dumps(origin.requests,indent=2))
        (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary))
    finally:
        if active is not None:
            active.kill();active.wait(timeout=10)
        origin.shutdown();origin.server_close()


if __name__=='__main__':main()
