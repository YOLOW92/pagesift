#!/usr/bin/env python3
"""Real CLI + local WordPress-protocol HTTP failure/restart acceptance."""
import hashlib,json,subprocess,sys,threading
from pathlib import Path
from http.server import BaseHTTPRequestHandler,HTTPServer
from urllib.parse import urlsplit,parse_qs
binary=Path(sys.argv[1]).resolve();out=Path(sys.argv[2]).resolve();out.mkdir(parents=True,exist_ok=False)
state={'updated':False,'fail_page2':False,'requests':[]}
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*args): pass
 def do_GET(self):
  if self.path=='/robots.txt':
   self.send_response(200);self.end_headers();self.wfile.write(b'User-agent: *\nAllow: /\n');return
  page=parse_qs(urlsplit(self.path).query).get('page',['1'])[0];state['requests'].append(page)
  if page=='2' and state['fail_page2']:
   self.send_response(503);self.end_headers();return
  rows=([{'id':1,'link':'/new'},{'id':2}] if state['updated'] else [{'id':1,'link':'/old'},{'id':2}] if page=='1' else [{'id':3}])
  self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('X-WP-TotalPages','1' if state['updated'] else '2');self.send_header('X-WP-Total','2' if state['updated'] else '3');self.end_headers();self.wfile.write(json.dumps(rows).encode())
server=HTTPServer(('127.0.0.1',0),Handler);thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
config=out/'config.toml';config.write_text('[fetch]\nallow_private_network=true\n[policy]\ninterval_ms=1\n')
events=out/'events.jsonl';url=f'http://127.0.0.1:{server.server_port}/wp-json/wp/v2/posts?per_page=2&page=1'
base=[str(binary),'--config',str(config),'collect',url,'--source','wp-posts','--data-dir',str(out/'state'),'--out',str(events),'--authoritative','--max-pages','1']
runs=[]
def run(ok=True):
 p=subprocess.run(base,capture_output=True,text=True,timeout=30);runs.append({'returncode':p.returncode,'stderr':p.stderr,'stdout':p.stdout});assert (p.returncode==0)==ok,p.stderr
try:
 run();assert len(events.read_text().splitlines())==2
 state['fail_page2']=True;run(False);assert len(events.read_text().splitlines())==2
 state['fail_page2']=False;run();assert len(events.read_text().splitlines())==3
 assert state['requests']==['1','2','2'],state
 state['updated']=True;run();rows=[json.loads(l) for l in events.open()];assert len(rows)==5
 assert rows[-2]['id']=='1' and rows[-2]['value']['link']=='/new'
 assert rows[-1]['id']=='3' and rows[-1]['value'] is None
 with events.open('ab') as f:f.write(b'{"interrupted":')
 run();assert len([json.loads(l) for l in events.open()])==5
 summary={'status':'passed','invocations':len(runs),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'requests':state['requests'],'events':rows,'checks':['page budget checkpoints','503 does not advance cursor','new process resumes page 2 without refetching page 1','URL churn updates same identity','terminal deletion only','unchanged scan emits nothing','partial event log tail repaired'],'runs':runs}
 (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps({k:v for k,v in summary.items() if k not in ('runs','events')}))
finally:server.shutdown();server.server_close();thread.join()
