#!/usr/bin/env python3
"""Complete self-hosted Firecrawl vs pagesift on the same frozen HTTP replay.

Quality diagnostic only: Docker bridge/worker budgets differ, so elapsed time is
not advertised as a performance comparison. All five reference containers count.
"""
import json
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.request
import urllib.error
from urllib.parse import urlsplit
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / 'bench/data/six'
STACK = [sys.executable, str(ROOT / 'bench/six/firecrawl_stack.py')]


def request(url, data=None, timeout=60):
    headers = {'Content-Type': 'application/json'}
    req = urllib.request.Request(url, data=json.dumps(data).encode() if data is not None else None, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as response:
            return response.status, json.load(response)
    except urllib.error.HTTPError as e:
        body = e.read().decode(errors='replace')
        try: body = json.loads(body)
        except json.JSONDecodeError: body = {'error': body[:2000]}
        return e.code, body


class Origin(BaseHTTPRequestHandler):
    def do_GET(self):
        path=urlsplit(self.path).path
        self.server.requests.append({'path': path, 'host': self.headers.get('Host'), 't': time.monotonic()})
        if path == '/robots.txt':
            body = b'User-agent: *\nAllow: /\n'
            mime = 'text/plain'
        elif path.startswith('/case/') and path[6:] in self.server.pages:
            body = Path(self.server.pages[path[6:]]['path']).read_bytes()
            mime = 'text/html; charset=utf-8'
        else:
            self.send_response(404); self.end_headers(); return
        self.send_response(200)
        self.send_header('Content-Type', mime)
        # Preserve identical HTML bytes, but do not run historical third-party
        # scripts, frames or media from an old corpus on today's web.
        self.send_header('Content-Security-Policy', "default-src 'none'; script-src 'none'; frame-src 'none'")
        self.send_header('Content-Length', str(len(body)))
        self.send_header('X-Pagesift-Replay', 'frozen-corpus')
        self.end_headers()
        try: self.wfile.write(body)
        except BrokenPipeError: pass

    def log_message(self, *_): pass


def main():
    out = Path(sys.argv[1]).resolve(); out.mkdir(parents=True, exist_ok=False)
    fixtures = [json.loads(l) for l in (DATA / 'fixtures.jsonl').read_text().splitlines()]
    articles = [json.loads(l) for l in (DATA / 'aeb.jsonl').read_text().splitlines()]
    rows = fixtures + articles
    # Bind the Docker host gateway rather than a public network interface.
    bridge = json.loads(subprocess.check_output(['docker','network','inspect','bridge'],text=True))[0]['IPAM']['Config'][0]['Gateway']
    origin = ThreadingHTTPServer((bridge, 0), Origin)
    origin.pages = {r['id']: r for r in rows}; origin.requests = []
    threading.Thread(target=origin.serve_forever,daemon=True).start()
    with socket.socket() as probe: probe.bind(('127.0.0.1',0)); port=probe.getsockname()[1]
    config = out/'pagesift.toml'
    config.write_text('[fetch]\nallow_private_network = true\n[policy]\ninterval_ms = 0\n'
                      '[cache]\nenabled = false\ndir = '+json.dumps(str(out/'store'))+'\n')
    pagesift = None
    log = (out/'stack.log').open('w')
    p_log = (out/'pagesift.log').open('w')
    stop = threading.Event()
    resource_log = (out/'docker-stats.jsonl').open('w')

    def sample():
        while not stop.is_set():
            try:
                ids=subprocess.check_output(['docker','ps','--filter','label=com.docker.compose.project=pagesift-six-firecrawl','--format','{{.ID}}'],text=True).split()
                if ids:
                    stats=subprocess.check_output(['docker','stats','--no-stream','--format','{{json .}}']+ids,text=True,timeout=10)
                    resource_log.write(json.dumps({'t':time.monotonic(),'containers':[json.loads(l) for l in stats.splitlines()]})+'\n');resource_log.flush()
            except Exception as e:
                resource_log.write(json.dumps({'sample_error':type(e).__name__})+'\n');resource_log.flush()
            stop.wait(3)

    def interrupted(*_):
        signal.signal(signal.SIGTERM,signal.SIG_IGN)
        raise KeyboardInterrupt()
    signal.signal(signal.SIGTERM, interrupted)
    observer=None
    results = {}
    try:
        subprocess.run(STACK+['up'],stdout=log,stderr=log,check=True,timeout=90)
        observer=threading.Thread(target=sample,daemon=True);observer.start()
        for i in range(180):
            if i % 3 == 0:
                state = subprocess.run(['docker','inspect','pagesift-six-firecrawl-api-1','--format','{{.State.Status}}'],capture_output=True,text=True)
                if state.stdout.strip() in ('exited','dead'):
                    raise RuntimeError('Firecrawl API exited during startup; inspect stack log')
            try:
                with urllib.request.urlopen('http://127.0.0.1:13992/',timeout=1) as r:
                    if r.status == 200: break
            except OSError: pass
            if shutil.disk_usage(ROOT).free < 10*2**30: raise RuntimeError('disk reserve')
            time.sleep(1)
        else: raise RuntimeError('Firecrawl startup deadline; see stack logs')
        pagesift=subprocess.Popen([str(DATA/'rust-target/release/pagesift'),'serve','--port',str(port),'--config',str(config)],stdout=p_log,stderr=p_log)
        for i in range(100):
            try:
                with urllib.request.urlopen(f'http://127.0.0.1:{port}/healthz',timeout=.5): break
            except OSError: time.sleep(.1)
        else: raise RuntimeError('pagesift startup deadline')
        # Wait for the queue workers, not just the API listener.
        warm_id=fixtures[0]['id']
        target=f'http://replay.pagesift.example.com:{origin.server_port}/case/{warm_id}'
        # Validate reachability and identity inside the actual API container.
        probe=subprocess.run(['docker','exec','pagesift-six-firecrawl-api-1','node','-e',
                              'fetch(process.argv[1]).then(async r=>{console.log(JSON.stringify({status:r.status,replay:r.headers.get("x-pagesift-replay"),body:(await r.text()).slice(0,200)}));if(r.status!==200||r.headers.get("x-pagesift-replay")!=="frozen-corpus")process.exitCode=1})',target],
                             capture_output=True,text=True,timeout=20)
        (out/'container-origin-probe.json').write_text(json.dumps({'exit':probe.returncode,'stdout':probe.stdout,'stderr':probe.stderr},indent=2)+'\n')
        if probe.returncode: raise RuntimeError('container cannot reach the verified replay origin')
        warm={'url':target,'formats':['markdown'],'timeout':10000,'maxAge':0}
        for i in range(12):
            code,doc=request('http://127.0.0.1:13992/v2/scrape',warm,timeout=20)
            (out/'warmup.json').write_text(json.dumps({'code':code,'response':doc},indent=2)+'\n')
            if code==200 and doc.get('success') and doc.get('data',{}).get('metadata',{}).get('statusCode')==200 and 'Image example' in doc.get('data',{}).get('markdown',''): break
            time.sleep(2)
        else: raise RuntimeError('Firecrawl scrape warmup failed')
        for name,api,host in [('pagesift',f'http://127.0.0.1:{port}',bridge),('firecrawl','http://127.0.0.1:13992','replay.pagesift.example.com')]:
            records=[]
            with (out/(name+'.jsonl')).open('w') as output:
                for i,row in enumerate(rows):
                    if shutil.disk_usage(ROOT).free < 10*2**30: raise RuntimeError('disk reserve')
                    started=time.monotonic()
                    options={'url':f'http://{host}:{origin.server_port}/case/{row["id"]}',
                             'formats':['markdown','images'] if row['id'].startswith('image-') else ['markdown'],
                             'onlyMainContent':row.get('only_main',True),'timeout':15000,'maxAge':0}
                    if name=='pagesift': options['engine']='http'
                    record={'id':row['id']}
                    try:
                        code,doc=request(api+'/v2/scrape',options,timeout=25)
                        data=doc.get('data') or {}
                        upstream=data.get('metadata',{}).get('statusCode')
                        record.update(status='ok' if code==200 and doc.get('success') and upstream==200 else 'error',http_status=code,upstream_status=upstream,
                                      markdown=data.get('markdown') or '',images=data.get('images') or [],response=doc,
                                      ms=(time.monotonic()-started)*1000)
                    except Exception as e:
                        record.update(status='transport_error',error=type(e).__name__+': '+str(e),markdown='',ms=(time.monotonic()-started)*1000)
                    output.write(json.dumps(record,ensure_ascii=False)+'\n');output.flush();records.append(record)
                    if i%20==0: print(name,i+1,'/',len(rows),record['status'],flush=True)
                    if len(records)>=5 and all(r['status']=='transport_error' for r in records[-5:]):
                        raise RuntimeError('five transport errors; infrastructure unavailable')
            results[name]={'n':len(records),'ok':sum(r['status']=='ok' for r in records)}
            (out/'summary.json').write_text(json.dumps(results,indent=2)+'\n')
        print(json.dumps(results))
    finally:
        (out/'origin-requests.json').write_text(json.dumps(origin.requests,indent=2)+'\n')
        stop.set()
        if observer: observer.join(timeout=12)
        resource_log.close()
        if pagesift:
            pagesift.terminate()
            try: pagesift.wait(timeout=10)
            except subprocess.TimeoutExpired: pagesift.kill();pagesift.wait()
        subprocess.run(STACK+['logs'],stdout=log,stderr=log,timeout=30)
        subprocess.run(STACK+['down'],stdout=log,stderr=log,timeout=60)
        origin.shutdown();origin.server_close();p_log.close();log.close()


if __name__=='__main__': main()
