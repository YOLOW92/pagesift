#!/usr/bin/env python3
"""Fresh seeded navigation graph; native and explicitly normalized scope tracks."""
import collections,json,os,re,subprocess,sys,threading,time
from pathlib import Path
from urllib.parse import urlsplit
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
ROOT=Path(__file__).resolve().parents[2]


def main():
    protocol=json.loads(Path(sys.argv[1]).read_text());out=Path(sys.argv[2]).resolve();out.mkdir(parents=True,exist_ok=False)
    prefix='/family-'+str(protocol['master_seed'])+'/'
    gold={prefix+s for s in ('','anchor','area','frame','htmx','data-htmx','html-next','header-next')}
    class Origin(BaseHTTPRequestHandler):
        def do_GET(self):self.reply('GET')
        def do_POST(self):self.reply('POST')
        def reply(self,method):
            path=urlsplit(self.path).path;self.server.requests.append({'path':path,'method':method})
            if path=='/robots.txt':
                body=('User-agent: *\nDisallow: '+prefix+'blocked\n').encode()
            elif path==prefix:
                body=(f'<html><head><link rel="next" href="html-next"><link rel="preload" href="asset"></head><body><article><h1>Index</h1>'
                    f'<a href="anchor#part">Read</a><map><area href="area"></map><iframe src="frame"></iframe>'
                    f'<div hx-get="htmx">More</div><div data-hx-get="data-htmx">Other</div>'
                    f'<a href="blocked">Denied</a><button hx-post="write">Write</button><img src="asset">'
                    f'</article></body></html>').encode()
            elif path in gold:body=('<article><h1>'+path+'</h1><p>'+'Distinct useful page content. '*80+'</p></article>').encode()
            else:
                self.send_response(404);self.end_headers();return
            self.send_response(200);self.send_header('Content-Type','text/html')
            if path==prefix:
                self.send_header('Link','<header-next?values=a,b>; title="one,two; three"; rel="next"')
                self.send_header('Link','<asset>; rel="preload", <ignored-context>; rel="prev"; anchor="/elsewhere"')
            self.end_headers();self.wfile.write(body)
        def log_message(self,*_):pass
    server=ThreadingHTTPServer(('127.0.0.1',0),Origin);server.requests=[]
    threading.Thread(target=server.serve_forever,daemon=True).start();url=f'http://127.0.0.1:{server.server_port}'+prefix
    cfg=out/'config.toml';cfg.write_text('[fetch]\nallow_private_network = true\n[policy]\ninterval_ms = 0\n[cache]\nenabled = false\n')
    tasks=[]
    for name,binary in zip(('baseline','candidate'),sys.argv[3:5],strict=True):tasks.append((name,[binary,'crawl',url,'--config',str(cfg),'--engine','http','--sitemap','skip','--limit','30','--depth','3']))
    tasks.append(('crawlee',['node','bench/six/adapters/crawlee.mjs',url,str(out/'crawlee-pages.json')]))
    katana=[str(ROOT/'bench/data/six/bin/katana'),'-u',url,'-d','3','-jc','-c','2','-p','1','-mdp','30','-ct','20s','-timeout','5','-retry','1','-silent','-nc','-jsonl','-duc','-fs','fqdn']
    tasks.append(('katana-native',katana))
    tasks.append(('katana-explicit-read-scope',katana+['-cos',re.escape(prefix)+'(?:blocked|write|asset|ignored-context)(?:$|\\?)']))
    summary={}
    try:
        for name,command in tasks:
            server.requests=[]
            env=dict(os.environ,CRAWLEE_STORAGE_DIR=str(out/'crawlee-store'),CRAWLEE_PURGE_ON_START='true')
            with (out/(name+'.stdout')).open('w') as stdout,(out/(name+'.stderr')).open('w') as stderr:
                proc=subprocess.run(command,stdout=stdout,stderr=stderr,env=env,timeout=90,check=True)
            visits=list(server.requests);seen={r['path'] for r in visits}
            summary[name]={'n':len(gold),'found':len(seen&gold),'missing':sorted(gold-seen),
                'robots_violations':sum(r['path']==prefix+'blocked' for r in visits),
                'write_requests':sum(r['method']!='GET' for r in visits),'requests':visits,'command':command}
            (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
        summary['candidate_passes']=summary['candidate']['found']==len(gold) and summary['candidate']['robots_violations']==summary['candidate']['write_requests']==0
        (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps({k:{a:b for a,b in v.items() if a not in ('requests','command')} if isinstance(v,dict) else v for k,v in summary.items()}))
    finally:server.shutdown();server.server_close()


if __name__=='__main__':main()
