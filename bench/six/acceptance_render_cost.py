#!/usr/bin/env python3
"""Measure the known extra-render tradeoff using engine choices, not speed ranks."""
import json,socket,subprocess,sys,threading,time,urllib.request
from pathlib import Path
from http.server import ThreadingHTTPServer,BaseHTTPRequestHandler
from firecrawl_full import request
ROOT=Path(__file__).resolve().parents[2]


def main():
    out=Path(sys.argv[1]).resolve();out.mkdir(parents=True,exist_ok=False)
    class Origin(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path=='/robots.txt':body=b'User-agent: *\nAllow: /\n'
            else:
                script='<script>window.telemetryOnly=1;</script>' if self.path.startswith('/analytics/') else '<script type="application/ld+json">{}</script>'
                body=('<article><h1>Brief static note</h1><p>The complete note is already here.</p></article>'+script).encode()
            self.send_response(200);self.send_header('Content-Type','text/html');self.end_headers();self.wfile.write(body)
        def log_message(self,*_):pass
    origin=ThreadingHTTPServer(('127.0.0.1',0),Origin);threading.Thread(target=origin.serve_forever,daemon=True).start()
    result={};active=None
    try:
        for name,folder in [('before','source'),('after','source-v2')]:
            with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
            cfg=out/(name+'.toml');cfg.write_text('[fetch]\nallow_private_network = true\n[policy]\ninterval_ms = 0\n[browser]\npath = "/opt/google/chrome/google-chrome"\nmax_browsers = 1\nmax_tabs_per_browser = 1\nmin_available_memory_mb = 1024\n[cache]\nenabled = false\ndir = '+json.dumps(str(out/(name+'-store')))+'\n')
            with (out/(name+'.log')).open('w') as log:
                active=subprocess.Popen([str(ROOT/f'bench/data/six/acceptance/{folder}/target/release/pagesift'),'serve','--port',str(port),'--config',str(cfg)],stdout=log,stderr=log)
                api=f'http://127.0.0.1:{port}'
                for _ in range(100):
                    try:
                        with urllib.request.urlopen(api+'/healthz',timeout=.5):break
                    except OSError:
                        if active.poll() is not None:raise RuntimeError('API exited')
                        time.sleep(.1)
                else:raise RuntimeError('API readiness')
                result[name]={};raw=[]
                for kind in ('analytics','json-data'):
                    engines=[]
                    for i in range(8):
                        code,doc=request(api+'/v2/scrape',{'url':f'http://127.0.0.1:{origin.server_port}/{kind}/{i}','engine':'auto','formats':['markdown']})
                        assert code==200 and 'complete note' in doc['data']['markdown']
                        engines.append(doc['data']['metadata']['pagesift']['engine']);raw.append(doc)
                    result[name][kind]={'n':8,'browser_renders':engines.count('browser'),'engines':engines}
                (out/(name+'.json')).write_text(json.dumps(raw,indent=2)+'\n')
                active.terminate();active.wait(timeout=15);active=None
        result['scope']='Controlled cost diagnostic, not held-out population performance: conservative client-code guard deliberately trades more renders for correctness.'
        (out/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))
    finally:
        if active is not None:active.kill();active.wait(timeout=10)
        origin.shutdown();origin.server_close()


if __name__=='__main__':main()
