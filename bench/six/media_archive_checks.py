#!/usr/bin/env python3
"""Native CLI validation, SIGKILL recovery, and fixed gallery-dl downloader contrast."""
import hashlib,io,json,os,subprocess,sys,threading,time,zipfile
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]

def payload(byte,n):
 out=io.BytesIO()
 with zipfile.ZipFile(out,'w',compression=zipfile.ZIP_STORED) as z:
  info=zipfile.ZipInfo('payload.dat',(2026,1,1,0,0,0));z.writestr(info,byte*n)
 return out.getvalue()

def main():
 out=Path(sys.argv[1]).resolve();out.mkdir(parents=True,exist_ok=False);binary=Path(sys.argv[2]).resolve()
 protocol={'scope':'controlled direct HTTP media integrity, not site extractor coverage or a speed benchmark','cases':['same-representation resume','changed-representation resume','Pagesift SIGKILL and archive-lock recovery'],'per_tool_retries':0,'prefix_bytes':786432,'binary':{'path':str(binary),'sha256':hashlib.sha256(binary.read_bytes()).hexdigest()},'gallery_source':json.loads((ROOT/'bench/data/six/sources/gallery-dl/.six-source.json').read_text()),'driver_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
 (out/'protocol.json').write_text(json.dumps(protocol,indent=2)+'\n')
 bodies=[payload(b'A',2*1024*1024),payload(b'B',2*1024*1024)]
 class Origin(BaseHTTPRequestHandler):
  def do_GET(self):
   self.server.events.append({'path':self.path,'range':self.headers.get('Range'),'if_range':self.headers.get('If-Range')})
   if self.path=='/robots.txt':
    self.send_response(200);self.end_headers();self.wfile.write(b'User-agent: *\nDisallow: /blocked\n');return
   phase=self.server.phase;body=bodies[phase];etag='"v'+str(phase+1)+'"';self.server.media_calls+=1
   start=0
   if self.headers.get('Range') and (not self.headers.get('If-Range') or self.headers.get('If-Range')==etag):start=int(self.headers['Range'].split('=')[1].split('-')[0])
   self.send_response(206 if start else 200);self.send_header('ETag',etag);self.send_header('Content-Type','application/zip');self.send_header('Content-Length',str(len(body)-start))
   if start:self.send_header('Content-Range',f'bytes {start}-{len(body)-1}/{len(body)}')
   self.end_headers();remaining=body[start:]
   if self.server.interrupt and self.server.media_calls==1:remaining=remaining[:protocol['prefix_bytes']]
   try:
    for i in range(0,len(remaining),16384):
     self.wfile.write(remaining[i:i+16384]);self.wfile.flush()
     if self.server.slow:time.sleep(.015)
   except (BrokenPipeError,ConnectionResetError):pass
   self.close_connection=True
  def log_message(self,*_):pass
 server=ThreadingHTTPServer(('127.0.0.1',0),Origin);server.daemon_threads=True;server.phase=0;server.interrupt=False;server.media_calls=0;server.slow=False;server.events=[]
 threading.Thread(target=server.serve_forever,daemon=True).start();url=f'http://127.0.0.1:{server.server_port}/media.zip'
 cfg=out/'pagesift.toml';cfg.write_text('[fetch]\nallow_private_network = true\ntotal_timeout_secs = 30\n[policy]\ninterval_ms = 0\n')
 # Config rejects unknown keys, so use only the documented existing fields.
 cfg.write_text('[fetch]\nallow_private_network = true\n[policy]\ninterval_ms = 0\n')
 env=dict(os.environ,PYTHONPATH=str(ROOT/'bench/data/six/sources/gallery-dl'))
 def command(tool,dest,ident='item'):
  if tool=='pagesift':return [str(binary),'download',url,'--config',str(cfg),'--source','controlled','--id',ident,'--out',str(dest),'--max-bytes',str(8*1024*1024)]
  return [str(ROOT/'bench/data/six/venv/bin/python'),'-m','gallery_dl','--config-ignore','--retries','0','-o','downloader.http.timeout=5','-D',str(dest),'-f','payload.zip',url]
 def run(cmd,path):
  p=subprocess.run(cmd,capture_output=True,text=True,env=env,timeout=30);path.with_suffix('.stdout').write_text(p.stdout);path.with_suffix('.stderr').write_text(p.stderr);return p
 summary={}
 try:
  for changed in [False,True]:
   case='changed' if changed else 'unchanged';summary[case]={}
   for tool in ['pagesift','gallery-dl']:
    server.phase=0;server.interrupt=True;server.media_calls=0;server.slow=False;server.events=[];dest=out/case/tool;dest.mkdir(parents=True)
    first=run(command(tool,dest),dest/'first');assert first.returncode!=0,(tool,first.stdout,first.stderr)
    server.phase=int(changed)
    second=run(command(tool,dest),dest/'second')
    files=list(dest.glob('*.bin')) if tool=='pagesift' else list(dest.glob('*.zip'))
    result={'first_exit':first.returncode,'second_exit':second.returncode,'finished_files':len(files),'matches_current_representation':len(files)==1 and files[0].read_bytes()==bodies[int(changed)],'requests':list(server.events),'commands':command(tool,dest)}
    summary[case][tool]=result
    assert sum(e['path']=='/media.zip' for e in server.events)>=2, 'tool did not exercise the HTTP downloader'
    if tool=='pagesift' or not changed:assert result['second_exit']==0 and result['matches_current_representation']
  server.phase=0;server.interrupt=False;server.media_calls=0;server.slow=True;server.events=[];dest=out/'kill';dest.mkdir();cmd=command('pagesift',dest,'kill')
  with (dest/'killed.stdout').open('w') as stdout,(dest/'killed.stderr').open('w') as stderr:
   proc=subprocess.Popen(cmd,stdout=stdout,stderr=stderr,env=env)
   try:
    deadline=time.monotonic()+20;saved=None
    while time.monotonic()<deadline:
     states=[p for p in dest.glob('*.json')]
     if states:
      saved=json.loads(states[0].read_text())
      if saved['bytes']>=1024*1024:break
     if proc.poll() is not None:raise RuntimeError('download exited before kill checkpoint')
     time.sleep(.025)
    else:raise RuntimeError('no durable checkpoint before deadline')
    concurrent=run(cmd,dest/'concurrent');assert concurrent.returncode!=0 and 'writer' in concurrent.stderr
    proc.kill();proc.wait(timeout=10)
   finally:
    if proc.poll() is None:proc.kill();proc.wait()
  checkpoint=json.loads(next(dest.glob('*.json')).read_text());resumed=run(cmd,dest/'resumed');assert resumed.returncode==0,resumed.stderr
  result=json.loads(resumed.stdout);assert result['resumed_bytes']==checkpoint['bytes'] and Path(result['path']).read_bytes()==bodies[0]
  summary['sigkill']={'exit':proc.returncode,'durable_bytes':checkpoint['bytes'],'resumed_bytes':result['resumed_bytes'],'matches_source':True,'concurrent_writer_rejected':True,'requests':list(server.events)}
 finally:server.shutdown();server.server_close()
 (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps({k:{t:{a:b for a,b in r.items() if a not in ('requests','commands')} for t,r in v.items()} if k!='sigkill' else {a:b for a,b in v.items() if a!='requests'} for k,v in summary.items()}))

if __name__=='__main__':main()
