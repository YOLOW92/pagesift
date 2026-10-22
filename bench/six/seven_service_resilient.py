#!/usr/bin/env python3
"""All 545 service pages with explicit fault isolation and no failed-page retry.
The reference implementation remains unchanged. A timed-out page cannot poison
every later page: restart this disposable stack before advancing to the next.
"""
import json,shutil,signal,socket,subprocess,sys,threading,time,urllib.request
from pathlib import Path
from http.server import ThreadingHTTPServer
import firecrawl_full as full
ROOT=Path(__file__).resolve().parents[2]
DATA=ROOT/'bench/data/six/seven-next/service-input'
OUT=ROOT/'bench/six/results/seven-next/service-isolated'
STACK=[sys.executable,str(ROOT/'bench/six/firecrawl_stack.py')]

def main():
 OUT.mkdir(parents=True,exist_ok=False)
 rows=[json.loads(l) for name in ['fixtures','aeb'] for l in (DATA/(name+'.jsonl')).read_text().splitlines()]
 bridge=json.loads(subprocess.check_output(['docker','network','inspect','bridge'],text=True))[0]['IPAM']['Config'][0]['Gateway']
 origin=ThreadingHTTPServer((bridge,0),full.Origin);origin.pages={r['id']:r for r in rows};origin.requests=[]
 threading.Thread(target=origin.serve_forever,daemon=True).start()
 with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
 binary=ROOT/'bench/six/results/seven-next/bin-verified/pagesift'
 cfg=OUT/'pagesift.toml';cfg.write_text('[server]\npersist_jobs=false\n[fetch]\nallow_private_network=true\n[policy]\ninterval_ms=0\n[cache]\nenabled=false\ndir='+json.dumps(str(OUT/'candidate-data'))+'\n')
 protocol=json.loads((DATA/'protocol.json').read_text());protocol.update(fault_isolation='Every failed/timeout page kept as empty output; no retry of that page. Before next page restart candidate service or full disposable reference stack+volumes. No reference code modified.',per_document_http_deadline_s=25,api_timeout_ms=15000,revision_reason='Prior full-service run stopped after five consecutive HTTP timeouts; its 75 reference records remain separate, not used as all-545 scores.')
 (OUT/'protocol.json').write_text(json.dumps(protocol,indent=2)+'\n')
 stop=threading.Event();samples=(OUT/'docker-stats.jsonl').open('w');stack_log=(OUT/'stack.log').open('w');candidate_log=(OUT/'pagesift.log').open('w');candidate=None;recoveries=[]
 def sample():
  while not stop.is_set():
   try:
    ids=subprocess.check_output(['docker','ps','--filter','label=com.docker.compose.project=pagesift-six-firecrawl','--format','{{.ID}}'],text=True).split()
    if ids:
     data=subprocess.check_output(['docker','stats','--no-stream','--format','{{json .}}']+ids,text=True,timeout=10)
     samples.write(json.dumps({'t':time.monotonic(),'containers':[json.loads(l) for l in data.splitlines()]})+'\n');samples.flush()
   except Exception as e:samples.write(json.dumps({'sample_error':str(e)})+'\n');samples.flush()
   stop.wait(3)
 observer=threading.Thread(target=sample,daemon=True);observer.start()
 def interrupted(*_):signal.signal(signal.SIGTERM,signal.SIG_IGN);raise KeyboardInterrupt()
 signal.signal(signal.SIGTERM,interrupted)
 def start_candidate():
  p=subprocess.Popen([str(binary),'serve','--port',str(port),'--config',str(cfg)],stdout=candidate_log,stderr=candidate_log)
  for _ in range(100):
   try:
    with urllib.request.urlopen(f'http://127.0.0.1:{port}/healthz',timeout=.5) as r:
     if r.status==200:return p
   except OSError:pass
   if p.poll() is not None:raise RuntimeError('candidate exited at readiness')
   time.sleep(.1)
  p.terminate();p.wait(timeout=10);raise RuntimeError('candidate readiness timeout')
 def stop_candidate(p):
  p.terminate()
  try:p.wait(timeout=10)
  except subprocess.TimeoutExpired:p.kill();p.wait()
 def stack(mode):subprocess.run(STACK+[mode],stdout=stack_log,stderr=stack_log,check=True,timeout=90)
 def start_reference():
  stack('up')
  for _ in range(180):
   try:
    with urllib.request.urlopen('http://127.0.0.1:13992/',timeout=1) as r:
     if r.status==200:break
   except OSError:time.sleep(1)
  else:raise RuntimeError('reference listener readiness timeout')
  warm={'url':f'http://replay.pagesift.example.com:{origin.server_port}/case/image-readiness-only','formats':['markdown'],'timeout':10000,'maxAge':0}
  for _ in range(12):
   try:
    code,doc=full.request('http://127.0.0.1:13992/v2/scrape',warm,timeout=20)
    if code==200 and doc.get('success') and 'Image example' in (doc.get('data') or {}).get('markdown',''):return
   except Exception:pass
   time.sleep(2)
  raise RuntimeError('reference scrape readiness timeout')
 summary={}
 try:
  candidate=start_candidate()
  for tool in ['pagesift','firecrawl']:
   if tool=='firecrawl':start_reference()
   need_recovery=False;records=[]
   with (OUT/(tool+'.jsonl')).open('w') as output:
    for i,row in enumerate(rows):
     if shutil.disk_usage(ROOT).free<10*2**30:raise RuntimeError('disk reserve')
     if need_recovery:
      began=time.monotonic()
      if tool=='pagesift':stop_candidate(candidate);candidate=start_candidate()
      else:
       subprocess.run(STACK+['logs'],stdout=stack_log,stderr=stack_log,timeout=30)
       stack('down');start_reference()
      recoveries.append({'tool':tool,'before_id':row['id'],'after_failure_id':records[-1]['id'],'seconds':time.monotonic()-began})
      (OUT/'recoveries.json').write_text(json.dumps(recoveries,indent=2)+'\n');need_recovery=False
     host=bridge if tool=='pagesift' else 'replay.pagesift.example.com'
     options={'url':f'http://{host}:{origin.server_port}/case/{row["id"]}','formats':['markdown'],'onlyMainContent':True,'timeout':15000,'maxAge':0}
     if tool=='pagesift':options['engine']='http'
     started=time.monotonic();record={'id':row['id']}
     try:
      api=f'http://127.0.0.1:{port}' if tool=='pagesift' else 'http://127.0.0.1:13992'
      code,doc=full.request(api+'/v2/scrape',options,timeout=25);data=doc.get('data') or {};status=data.get('metadata',{}).get('statusCode');ok=code==200 and doc.get('success') and status==200
      record.update(status='ok' if ok else 'error',http_status=code,upstream_status=status,markdown=(data.get('markdown') or '') if ok else '',response=doc)
     except Exception as e:record.update(status='transport_error',error=type(e).__name__+': '+str(e),markdown='')
     record['ms']=(time.monotonic()-started)*1000;output.write(json.dumps(record,ensure_ascii=False)+'\n');output.flush();records.append(record)
     need_recovery=record['status']!='ok'
     if i%20==0 or need_recovery:print(json.dumps({'tool':tool,'done':i+1,'total':len(rows),'id':row['id'],'status':record['status']}),flush=True)
   summary[tool]={'n':len(records),'ok':sum(r['status']=='ok' for r in records)};(OUT/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
  (OUT/'recoveries.json').write_text(json.dumps(recoveries,indent=2)+'\n')
 finally:
  (OUT/'origin-requests.json').write_text(json.dumps(origin.requests,indent=2)+'\n')
  stop.set();observer.join(timeout=12);samples.close()
  if candidate:stop_candidate(candidate)
  subprocess.run(STACK+['logs'],stdout=stack_log,stderr=stack_log,timeout=30)
  subprocess.run(STACK+['down'],stdout=stack_log,stderr=stack_log,timeout=90)
  origin.shutdown();origin.server_close();stack_log.close();candidate_log.close()
 print(json.dumps(summary))

if __name__=='__main__':main()
