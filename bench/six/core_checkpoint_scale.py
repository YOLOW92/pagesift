#!/usr/bin/env python3
"""Bounded payload-volume regression: real CLI, 1000 x 64 KiB items over 10 HTTP pages."""
import hashlib,json,subprocess,sys,threading
from pathlib import Path
from http.server import BaseHTTPRequestHandler,HTTPServer
from urllib.parse import urlsplit,parse_qs
out=Path(sys.argv[1]).resolve();out.mkdir(parents=True,exist_ok=False)
base=Path(sys.argv[2]).resolve();new=Path(sys.argv[3]).resolve()
n=1000;page_size=100;body='independent item content '*2800
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def do_GET(self):
  self.send_response(200);self.send_header('Content-Type','application/json');self.end_headers()
  if self.path=='/robots.txt':self.wfile.write(b'User-agent: *\nAllow: /\n');return
  page=int(parse_qs(urlsplit(self.path).query).get('page',['1'])[0]);rows=[{'id':str(i),'body':body} for i in range((page-1)*page_size,min(page*page_size,n))]
  self.wfile.write(json.dumps({'items':rows,'next':f'?page={page+1}' if page*page_size<n else None}).encode())
server=HTTPServer(('127.0.0.1',0),Handler);thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
config=out/'config.toml';config.write_text('[fetch]\nallow_private_network=true\n[policy]\ninterval_ms=1\n');adapter=out/'adapter.json';adapter.write_text(json.dumps({'items_pointer':'/items','id_pointer':'/id','next_pointer':'/next'}))
results={}
def command(binary,dest):return [str(binary),'--config',str(config),'collect',f'http://127.0.0.1:{server.server_port}/items?page=1','--source','scale','--adapter',str(adapter),'--data-dir',str(dest/'state'),'--out',str(dest/'events.jsonl'),'--max-pages','10']
try:
 for label,binary in [('baseline',base),('candidate',new)]:
  dest=out/label;dest.mkdir();cmd=command(binary,dest);p=subprocess.run(cmd,capture_output=True,text=True,timeout=90)
  events=sum(1 for _ in (dest/'events.jsonl').open())
  result={'returncode':p.returncode,'stderr':p.stderr,'events':events,'state_journal_bytes':sum(p.stat().st_size for p in (dest/'state').rglob('*') if p.is_file()),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest()}
  if label=='baseline':assert p.returncode!=0 and '32 MiB' in p.stderr
  else:
   assert p.returncode==0 and events==n,p.stderr
   again=subprocess.run(cmd,capture_output=True,text=True,timeout=90);assert again.returncode==0,again.stderr
   assert sum(1 for _ in (dest/'events.jsonl').open())==n
   result['unchanged_rescan_no_duplicate_events']=True
  results[label]=result
 (out/'summary.json').write_text(json.dumps({'n':n,'pages':10,'body_bytes_each':len(body.encode()),'payload_bytes':n*len(body.encode()),'results':results,'scope':'functional state-capacity regression; not a throughput comparison'},indent=2)+'\n');print(json.dumps(results))
finally:server.shutdown();server.server_close();thread.join()
