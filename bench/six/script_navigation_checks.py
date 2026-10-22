#!/usr/bin/env python3
"""Fixed same-input script navigation contrast, including rejected candidates."""
import hashlib,json,secrets,subprocess,sys,threading
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit
ROOT=Path(__file__).resolve().parents[2]
def main():
 out=Path(sys.argv[1]).resolve();out.mkdir(parents=True,exist_ok=False);prefix='/nav-'+secrets.token_hex(5)+'/'
 gold={prefix+p for p in ['', 'anchor','location','assign','replace','open','read','escaped']}
 negatives={prefix+p for p in ['post','unknown','comment','string','dynamic','unproven','data-only','regex']}
 commands={k:[str(Path(p).resolve())] for k,p in zip(['before','after'],sys.argv[2:4],strict=True)}
 protocol={'scope':'literal inline navigation/GET discovery only; not all JS endpoints, no AST evaluation or external bundle traversal','prefix':prefix,'gold':sorted(gold),'negatives':sorted(negatives),'binaries':{k:{'path':cmd[0],'sha256':hashlib.sha256(Path(cmd[0]).read_bytes()).hexdigest()} for k,cmd in commands.items()},'katana_source':json.loads((ROOT/'bench/data/six/sources/katana/.six-source.json').read_text())};(out/'protocol.json').write_text(json.dumps(protocol,indent=2)+'\n')
 class Origin(BaseHTTPRequestHandler):
  def do_GET(self):
   p=urlsplit(self.path).path;self.server.events.append({'path':p,'method':'GET'})
   if p=='/robots.txt':body=b'User-agent: *\nDisallow: /blocked\n'
   elif p==prefix:
    body=(f"<title>Navigation index</title><article><h1>Navigation index</h1><a href='anchor'>Visible link</a></article>"
     f"<script>window.location.href='{prefix}location';location.assign('{prefix}assign');location.replace('{prefix}replace');window.open('{prefix}open');fetch('{prefix}read');fetch('\\u002f{prefix[1:]}escaped');"
     f"fetch('{prefix}post',{{method:'POST'}});fetch('{prefix}unknown',options);const unused='{prefix}unproven';"
     f"/* fetch('{prefix}comment'); */const text=\"fetch('{prefix}string')\";const r=/fetch('{prefix.replace('/',chr(92)+'/')}regex')/;fetch('{prefix}dynamic'+token);</script>"
     f"<script type='application/ld+json'>{{\"value\":\"fetch('{prefix}data-only')\"}}</script>").encode()
   elif p in gold:body=(f'<article><h1>{p}</h1><p>'+'Complete distinct destination text. '*30+'</p></article>').encode()
   else:self.send_response(404);self.end_headers();return
   self.send_response(200);self.send_header('Content-Type','text/html');self.end_headers();self.wfile.write(body)
  def do_POST(self):self.server.events.append({'path':urlsplit(self.path).path,'method':'POST'});self.send_response(405);self.end_headers()
  def log_message(self,*_):pass
 srv=ThreadingHTTPServer(('127.0.0.1',0),Origin);srv.events=[];threading.Thread(target=srv.serve_forever,daemon=True).start();url=f'http://127.0.0.1:{srv.server_port}'+prefix
 cfg=out/'pagesift.toml';cfg.write_text('[fetch]\nallow_private_network = true\n[policy]\ninterval_ms = 0\n[cache]\nenabled = false\n')
 for cmd in commands.values():cmd.extend(['crawl',url,'--config',str(cfg),'--engine','http','--sitemap','skip','--limit','30','--depth','3'])
 commands['katana']=[str(ROOT/'bench/data/six/bin/katana'),'-u',url,'-d','3','-jc','-c','2','-p','1','-mdp','30','-ct','10s','-timeout','3','-retry','0','-silent','-nc','-jsonl','-duc','-fs','fqdn']
 summary={}
 try:
  for label,cmd in commands.items():
   srv.events=[]
   p=subprocess.run(cmd,capture_output=True,text=True,timeout=45);(out/(label+'.stdout')).write_text(p.stdout);(out/(label+'.stderr')).write_text(p.stderr);assert p.returncode==0,(label,p.stderr)
   events=list(srv.events);seen={r['path'] for r in events}
   summary[label]={'found':len(gold&seen),'expected':len(gold),'missing':sorted(gold-seen),'rejected_candidates_visited':sorted(negatives&seen),'non_get_requests':sum(r['method']!='GET' for r in events),'requests':events,'command':cmd}
  assert summary['after']['found']==len(gold) and not summary['after']['rejected_candidates_visited'] and summary['after']['non_get_requests']==0
 finally:srv.shutdown();srv.server_close()
 (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps({k:{a:b for a,b in v.items() if a not in ('requests','command')} for k,v in summary.items()}))
if __name__=='__main__':main()
