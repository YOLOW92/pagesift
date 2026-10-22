#!/usr/bin/env python3
"""Pre-frozen mixed-page families, including shared-path/query counterexamples."""
import html,json,os,socket,subprocess,sys,threading,time,urllib.request
from pathlib import Path
from urllib.parse import urlsplit
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from firecrawl_full import request

ROOT=Path(__file__).resolve().parents[2]


class Origin(BaseHTTPRequestHandler):
    def do_GET(self):
        self.server.requests.append(self.path)
        if self.path=='/robots.txt':body=b'User-agent: *\nAllow: /\n'
        else:
            case=self.server.pages.get(self.path)
            if case is None:self.send_response(404);self.end_headers();return
            if case.get('javascript'):
                body=case['javascript'].encode()
            elif case.get('answer'):
                content='<h1>Independent application view</h1><p>'+(' '+case['answer'])*50+'</p>'
                action='document.querySelector("article").innerHTML='+json.dumps(content)+';'
                if case['kind']=='delayed-route':action='setTimeout(()=>{'+action+'},75);'
                wrapper='<article><h1>Awaiting client view</h1></article>'
                if case['kind']=='external-script':
                    path='/assets/'+case['slug']+'.js'
                    self.server.pages[path]={'javascript':action}
                    body=(wrapper+'<script src="'+path+'"></script>').encode()
                elif case['kind']=='body-onload':
                    body=('<body onload="'+html.escape(action,quote=True)+'">'+wrapper+'</body>').encode()
                elif case['kind']=='json-data-only':
                    body=('<article>'+content+'</article><script type="application/ld+json">{}</script>').encode()
                elif case['kind']=='module-script':
                    body=(wrapper+'<script type="module">'+action+'</script>').encode()
                else:body=(wrapper+'<script>'+action+'</script>').encode()
            else:body=b'<article><h1>Brief note</h1><p>This static note is complete.</p></article>'
        self.send_response(200);self.send_header('Content-Type','application/javascript' if self.path.endswith('.js') else 'text/html; charset=utf-8');self.end_headers()
        try:self.wfile.write(body)
        except BrokenPipeError:pass
    def log_message(self,*_):pass


def main():
    protocol=Path(sys.argv[1]).resolve();out=Path(sys.argv[2]).resolve();out.mkdir(parents=True,exist_ok=False)
    families=json.loads(protocol.read_text())['adaptive_families']
    origin=ThreadingHTTPServer(('127.0.0.1',0),Origin);origin.pages={};origin.requests=[]
    base=f'http://127.0.0.1:{origin.server_port}'
    urls=[];gold={}
    for f in families:
        slug=f['slug'];kind=f['kind']
        static=[f'/{slug}/notes/{i}' for i in range(4)]
        dynamic=f'/{slug}/application'
        if kind=='different-segment':dynamic=f'/{slug}app/detail'
        if kind=='query-route':
            static=[f'/{slug}/view?mode=static&i={i}' for i in range(4)]
            dynamic=f'/{slug}/view?mode=dynamic'
        for path in static:origin.pages[path]={'kind':kind};urls.append(base+path)
        origin.pages[dynamic]={'kind':kind,'answer':f['answer'],'slug':slug};urls.append(base+dynamic)
        gold[base+dynamic]=f
    threading.Thread(target=origin.serve_forever,daemon=True).start()
    summary={};active=None
    try:
        for name,binary in zip(('baseline','candidate'),sys.argv[3:5],strict=True):
            run=out/name;run.mkdir()
            with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
            cfg=run/'config.toml';cfg.write_text('[fetch]\nallow_private_network = true\n[policy]\ninterval_ms = 0\n[browser]\npath = "/opt/google/chrome/google-chrome"\nmax_browsers = 1\nmax_tabs_per_browser = 1\nmin_available_memory_mb = 1024\n[cache]\nenabled = false\ndir = '+json.dumps(str(run/'store'))+'\n')
            with (run/'service.log').open('w') as log:
                active=subprocess.Popen([binary,'serve','--port',str(port),'--config',str(cfg)],stdout=log,stderr=log)
                api=f'http://127.0.0.1:{port}'
                for _ in range(100):
                    try:
                        with urllib.request.urlopen(api+'/healthz',timeout=.5):break
                    except OSError:
                        if active.poll() is not None:raise RuntimeError('API exited')
                        time.sleep(.1)
                else:raise RuntimeError('API readiness')
                records=[]
                for url in urls:
                    status,response=request(api+'/v2/scrape',{'url':url,'engine':'auto','formats':['markdown'],'timeout':30000},timeout=40)
                    records.append({'url':url,'http':status,'response':response})
                (run/'responses.json').write_text(json.dumps(records,indent=2)+'\n')
                summary[name]=[{'kind':gold[r['url']]['kind'],'answer_found':gold[r['url']]['answer'] in r['response'].get('data',{}).get('markdown',''),
                                'url':r['url'],'http':r['http'],'warning':r['response'].get('data',{}).get('warning')} for r in records if r['url'] in gold]
                active.terminate();active.wait(timeout=15);active=None
            (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
        manifest=out/'urls.json';manifest.write_text(json.dumps(urls))
        env=dict(os.environ,CRAWLEE_STORAGE_DIR=str(out/'crawlee-storage'),CRAWLEE_PURGE_ON_START='true')
        with (out/'crawlee.log').open('w') as log:
            p=subprocess.run(['node','bench/six/adapters/adaptive.mjs',str(manifest),str(out/'crawlee.json')],env=env,stdout=log,stderr=log,timeout=180)
        if p.returncode:raise RuntimeError('Crawlee failed')
        items=json.loads((out/'crawlee.json').read_text())['items'];by_url={r['url']:r for r in items}
        summary['crawlee']=[{'kind':f['kind'],'answer_found':f['answer'] in by_url.get(u,{}).get('text',''),'url':u} for u,f in gold.items()]
        summary['candidate_passes_frozen_threshold']=all(r['answer_found'] for r in summary['candidate'])
        summary['protocol_sha256']=__import__('hashlib').sha256(protocol.read_bytes()).hexdigest()
        (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
        (out/'origin-requests.json').write_text(json.dumps(origin.requests,indent=2)+'\n')
        print(json.dumps(summary))
        # Quality failures remain scored observations, not infrastructure errors.
    finally:
        if active is not None:active.kill();active.wait(timeout=10)
        origin.shutdown();origin.server_close()


if __name__=='__main__':main()
