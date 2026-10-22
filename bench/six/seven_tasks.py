#!/usr/bin/env python3
"""Fresh controlled native-CLI tasks. Synthetic fixtures are not a blind web study.
All cases and gold are persisted before the first tool runs; failures stay in
the results. No reference algorithm is reimplemented in a test adapter.
"""
import base64,hashlib,json,os,secrets,subprocess,sys,tempfile,threading,time
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit
ROOT=Path(__file__).resolve().parents[2]
OUT=ROOT/'bench/six/results/seven-next/tasks'
BIN=ROOT/'bench/six/results/seven-next/bin-verified/pagesift'
PY=ROOT/'bench/data/six/venv/bin/python'

def save(p,value):p.write_text(json.dumps(value,indent=2,ensure_ascii=False)+'\n')
def digest(p):return hashlib.sha256(Path(p).read_bytes()).hexdigest()
def run(label,cmd,timeout=120,env=None):
 start=time.monotonic()
 try:
  p=subprocess.run(list(map(str,cmd)),capture_output=True,text=True,timeout=timeout,env=env)
  result={'command':list(map(str,cmd)),'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr,'wall_s':time.monotonic()-start}
 except Exception as e:result={'command':list(map(str,cmd)),'error':str(e),'exit':None,'stdout':'','stderr':'','wall_s':time.monotonic()-start}
 save(OUT/(label+'.json'),result);return result

def main():
 global OUT
 if len(sys.argv)>1:OUT=Path(sys.argv[1]).resolve()
 OUT.mkdir(parents=True,exist_ok=False);seed=secrets.token_hex(10);prefix='/fresh-'+seed+'/'
 fixtures={};gold={prefix,*(prefix+x for x in ['anchor','frame','htmx','inline','literal','concat','template','escaped','imported','exported','dynamic','header'])}
 rejected={prefix+x for x in ['post','unused','comment','data']}
 root=(f"<article><h1>Independent navigation {seed}</h1><a href='{prefix}anchor'>Anchor</a><iframe src='{prefix}frame'></iframe><div hx-get='{prefix}htmx'></div></article>"
       f"<script>fetch('{prefix}inline');/*fetch('{prefix}comment');*/</script><script type='application/ld+json'>{{\"url\":\"{prefix}data\"}}</script>"
       f"<script src='{prefix}assets/app.js'></script><script src='{prefix}assets/dynamic.js'></script>")
 fixtures[prefix]=('text/html',root.encode())
 fixtures[prefix+'assets/app.js']=('application/javascript',(f"import './child.js';export {{x}} from './other.js';const ROOT='{prefix}';fetch(ROOT+'concat');fetch('{prefix}literal');fetch(`{prefix}template`);fetch('\\u002f{prefix[1:]}escaped');fetch('{prefix}post',{{method:'POST'}});const unused='{prefix}unused';").encode())
 fixtures[prefix+'assets/child.js']=('application/javascript',(f"import './app.js';fetch('{prefix}imported');").encode())
 fixtures[prefix+'assets/other.js']=('application/javascript',(f"location.replace('{prefix}exported');").encode())
 fixtures[prefix+'assets/dynamic.js']=('application/javascript',(f"const join=(a,b)=>a+b;fetch(join('{prefix}','dynamic'));").encode())
 for p in gold-{prefix}:fixtures[p]=('text/html',(f'<article><h1>{p.rsplit("/",1)[-1]}</h1><p>Destination {p}, frozen independent navigation content.</p></article>').encode())
 # Appearance-only relocation: both tools see the same page pairs. One tag
 # change per family intentionally tests a known unsupported Pagesift case.
 relocation=[]
 for i in range(8):
  name=f'Copper kettle model {seed[:6]}{i}';old=f"<main class='catalog'><article id='target' class='product'><h2>{name}</h2><p>Price 42</p></article></main>"
  changed=old.replace("id='target'",'')
  cases=[('unchanged',old,name+'Price 42'),('id-removed',changed,name+'Price 42'),('wrapper','<section>'+changed+'</section>',name+'Price 42'),('class-changed',changed.replace("class='product'","class='card'"),name+'Price 42'),('price-changed',changed.replace('Price 42','Price 43'),name+'Price 43'),('tag-changed',changed.replace('article','section'),name+'Price 42'),('removed','<main><p>No products remain.</p></main>',None),('duplicate',changed+changed,None)]
  for kind,html,wanted in cases:relocation.append({'id':f'{i}-{kind}','old':old,'new':html,'wanted':wanted,'kind':kind})
 (OUT/'relocation').mkdir()
 for r in relocation:
  (OUT/'relocation'/(r['id']+'-old.html')).write_text(r['old']);(OUT/'relocation'/(r['id']+'-new.html')).write_text(r['new'])
 # Body-only indicator/form gold, with fake, non-live credentials.
 pages=[]
 for i in range(12):
  key='AKIA'+hashlib.sha256((seed+str(i)).encode()).hexdigest()[:16].upper()
  body=f"<article><h1>Fixture {i}</h1></article><form id='f{i}' action='/search' method='get'><label for='q{i}'>Search</label><input id='q{i}' name='q' required><input type='hidden' name='csrf' value='PRIVATE-{seed}'><select name='kind'><option selected>article</option></select></form><script src='/wp-content/a.js'></script><script>const fixture_key='{key}';</script>"
  p=OUT/f'inspect-{i}.html';p.write_text(body);pages.append({'path':str(p),'form_count':1,'control_count':3,'technology':'wordpress','secret_indicator':'aws_access_key_id'})
  fixtures[f'/inspect/{i}']=('text/html',body.encode())
 fixtures['/inspect/']=('text/html',(''.join(f'<a href="/inspect/{i}">Case {i}</a>' for i in range(12))).encode())
 # Actual stream fixtures are authored with the already-installed ffmpeg solely
 # for testing. Neither ffmpeg nor yt-dlp is a product runtime dependency.
 streams=OUT/'streams';streams.mkdir();(streams/'hls').mkdir();(streams/'dash').mkdir()
 frequency=400+int(seed[:3],16)%500
 common=['ffmpeg','-hide_banner','-loglevel','error','-f','lavfi','-i','testsrc2=size=64x64:rate=5','-f','lavfi','-i',f'sine=frequency={frequency}:sample_rate=44100','-t','2','-c:v','libx264','-threads','1','-g','5','-c:a','aac','-b:a','32k']
 subprocess.run(common+['-f','hls','-hls_time','1','-hls_list_size','0','-hls_playlist_type','vod',str(streams/'hls/index.m3u8')],check=True,timeout=60)
 subprocess.run(common+['-f','dash','-seg_duration','1',str(streams/'dash/index.mpd')],check=True,timeout=60)
 for p in streams.rglob('*'):
  if p.is_file():fixtures['/streams/'+str(p.relative_to(streams))]=('application/octet-stream',p.read_bytes())
 # One native Mastodon status family, eight posts and 20 original attachments.
 png=base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/lN0AAAAASUVORK5CYII=')
 statuses={};media_gold={}
 for i in range(8):
  ident=str(1000+i);attachments=[]
  for j in range(i%4+1):
   mid=str(10_000+i*10+j);path=f'/files/{mid}.png';payload=png+(seed+mid).encode();fixtures[path]=('image/png',payload);media_gold[mid]=hashlib.sha256(payload).hexdigest()
   attachments.append({'id':mid,'type':'image','url':'__BASE__'+path,'preview_url':'__BASE__/preview/'+mid+'.png','description':'Original '+mid,'meta':{'original':{'width':1,'height':1}}})
  statuses[ident]={'id':ident,'media_attachments':attachments,'reblog':None,'in_reply_to_id':None,'account':{'id':'7','acct':'fixture','username':'fixture'},'tags':[],'created_at':'2026-10-01T00:00:00.000Z','content':'Controlled fixture','sensitive':False}
 events=[]
 class Handler(BaseHTTPRequestHandler):
  def do_GET(self):
   path=urlsplit(self.path).path;events.append({'path':path,'method':'GET'})
   if path=='/robots.txt':mime,body='text/plain',b'User-agent: *\nAllow: /\n'
   elif path.startswith('/api/v1/statuses/') and path.rsplit('/',1)[-1] in statuses:mime,body='application/json',json.dumps(statuses[path.rsplit('/',1)[-1]]).replace('__BASE__',base).encode()
   elif path in fixtures:mime,body=fixtures[path]
   else:self.send_response(404);self.end_headers();return
   self.send_response(200);self.send_header('Content-Type',mime);self.send_header('Content-Length',str(len(body)));self.send_header('ETag','"fixture-v1"')
   if path==prefix:self.send_header('Link',f'<{prefix}header>; rel=next')
   self.end_headers()
   try:self.wfile.write(body)
   except BrokenPipeError:pass
  def do_POST(self):events.append({'path':self.path,'method':'POST'});self.send_response(405);self.end_headers()
  def log_message(self,*_):pass
 server=ThreadingHTTPServer(('127.0.0.1',0),Handler);base=f'http://127.0.0.1:{server.server_port}';threading.Thread(target=server.serve_forever,daemon=True).start()
 cfg=OUT/'pagesift.toml';cfg.write_text('[fetch]\nallow_private_network=true\n[policy]\ninterval_ms=0\n[cache]\nenabled=false\n')
 fixture_dir=OUT/'http-fixtures';fixture_dir.mkdir()
 for index,(path,(mime,body)) in enumerate(fixtures.items()):(fixture_dir/str(index)).write_bytes(body)
 save(OUT/'http-fixture-index.json',{path:{'file':str(index),'content_type':mime} for index,(path,(mime,_)) in enumerate(fixtures.items())})
 save(OUT/'mastodon-statuses.json',statuses)
 protocol={'seed':seed,'scope':'fresh controlled cases, NOT independently authored blind site validation or full feature dominance','frozen_before_first_output':True,'navigation_gold':sorted(gold),'non_navigation_candidates':sorted(rejected),'navigation_reference_modes':['Katana -jc -jsl','Crawlee native CheerioCrawler + enqueueLinks'],'relocation':relocation,'relocation_thresholds':{'pagesift_score':0.72,'pagesift_margin':0.12,'scrapling':'native default 40 percent, CSS then adaptive fallback'},'inspect_gold':pages,'media_original_sha256':media_gold,'fixture_sha256':{p:hashlib.sha256(body).hexdigest() for p,(_,body) in fixtures.items()},'stream_source_sha256':{str(p.relative_to(streams)):digest(p) for p in streams.rglob('*') if p.is_file()},'binary_sha256':digest(BIN),'driver_sha256':digest(__file__),'references':json.loads((ROOT/'bench/six/tools.lock.json').read_text())['tools'],'browser_interact_and_agent':'excluded by direct user instruction'}
 save(OUT/'protocol.json',protocol);summary={}
 try:
  commands={'pagesift':[BIN,'crawl',base+prefix,'--config',cfg,'--engine','http','--external-scripts','--sitemap','skip','--limit','100','--depth','4'],
    'katana':[ROOT/'bench/data/six/bin/katana','-u',base+prefix,'-d','4','-jc','-jsl','-c','2','-p','1','-ct','20s','-timeout','5','-retry','0','-silent','-nc','-jsonl','-duc','-fs','fqdn'],
    'crawlee':['node',ROOT/'bench/six/adapters/crawlee.mjs',base+prefix,OUT/'crawlee-output.json']}
  nav={}
  for tool,cmd in commands.items():
   env=dict(os.environ,CRAWLEE_STORAGE_DIR=str(OUT/'crawlee-storage')) if tool=='crawlee' else None
   events.clear();result=run('navigation-'+tool,cmd,90,env);seen={r['path'] for r in events};nav[tool]={'exit':result['exit'],'found':len(gold&seen),'expected':len(gold),'missing':sorted(gold-seen),'non_navigation_visited':sorted(rejected&seen),'non_get':sum(r['method']!='GET' for r in events),'requests':list(events)}
  summary['navigation']=nav;save(OUT/'summary.json',summary)
  from scrapling import Selector
  reloc=[]
  with tempfile.TemporaryDirectory(prefix='pagesift-fresh-fuzzy-') as tmp:
   for r in relocation:
    ident=r['id'];a=OUT/'relocation'/(ident+'-old.html');b=OUT/'relocation'/(ident+'-new.html')
    result=run('fuzzy-'+ident,[BIN,'relocate',b,'--original',a,'--selector','#target'])
    try:
     value=json.loads(result['stdout']);got=value.get('text') if value.get('status')=='candidate' else None
    except Exception:got=None;value={'status':'error'}
    opts={'url':'https://example.test/'+ident+'/','adaptive':True,'storage_args':{'storage_file':str(Path(tmp)/(ident+'.db'))}}
    Selector(r['old'],**opts).css('#target',auto_save=True);page=Selector(r['new'],**opts);nodes=page.css('#target')
    if not nodes:nodes=page.css('#target',adaptive=True)
    values=[n.get_all_text(separator=' ',strip=True) for n in nodes]
    norm=lambda s:''.join(s.split()) if s is not None else None
    wanted=r['wanted'];scrapling_correct=[norm(v) for v in values]==([norm(wanted)] if wanted is not None else [])
    reloc.append({'id':ident,'kind':r['kind'],'gold':wanted,'pagesift':value,'pagesift_correct':norm(got)==norm(wanted),'scrapling_values':values,'scrapling_correct':scrapling_correct})
  save(OUT/'relocation-results.json',reloc);summary['relocation']={'n':len(reloc),'pagesift_correct':sum(r['pagesift_correct'] for r in reloc),'scrapling_correct':sum(r['scrapling_correct'] for r in reloc),'details':'relocation-results.json'};save(OUT/'summary.json',summary)
  inspection=[]
  for i,r in enumerate(pages):
   result=run('inspect-'+str(i),[BIN,'inspect',r['path'],'--base-url',base+prefix])
   try:
    report=json.loads(result['stdout']);rules={f['rule'] for f in report['knowledge']['findings']};ok=len(report['forms'])==r['form_count'] and len(report['forms'][0]['controls'])==r['control_count'] and r['technology'] in rules and r['secret_indicator'] in rules and 'PRIVATE-'+seed not in result['stdout']
   except Exception:ok=False
   inspection.append({'id':i,'ok':ok})
  summary['inspection']={'n':len(inspection),'passed':sum(r['ok'] for r in inspection),'scope':'Pagesift body/form gold only; not native Katana knowledge-base superiority'};save(OUT/'summary.json',summary)
  kb=run('katana-knowledge',[ROOT/'bench/data/six/bin/katana','-u',base+'/inspect/','-d','2','-kb','-kb-secrets','-td','-fx','-c','2','-timeout','5','-retry','0','-ct','20s','-silent','-nc','-jsonl','-duc','-fs','fqdn'],90)
  kb_rows=[]
  for line in kb['stdout'].splitlines():
   try:kb_rows.append(json.loads(line))
   except json.JSONDecodeError:pass
  matched={urlsplit(r.get('request',{}).get('endpoint','')).path:r for r in kb_rows}
  summary['katana_knowledge']={'exit':kb['exit'],'expected_pages':12,'pages_with_forms':sum(bool(matched.get('/inspect/'+str(i),{}).get('response',{}).get('forms')) for i in range(12)),'pages_with_secret_findings':sum(bool(matched.get('/inspect/'+str(i),{}).get('response',{}).get('knowledgebase',{}).get('secrets')) for i in range(12)),'scope':'syntactic fake-key indicators only, no live validation; different rule coverage, no full scanner ranking'};save(OUT/'summary.json',summary)
  media={};environment=dict(os.environ,PYTHONPATH=str(ROOT/'bench/data/six/sources/gallery-dl'))
  for tool in ['pagesift','gallery-dl']:
   dest=OUT/('media-'+tool);dest.mkdir();records=[]
   for ident in statuses:
    if tool=='pagesift':cmd=[BIN,'--config',cfg,'media-archive',base+'/api/v1/statuses/'+ident,'--adapter','mastodon','--out',dest,'--template','{id}.{ext}']
    else:cmd=[PY,'-m','gallery_dl','--config-ignore','--retries','0','-o','extractor.mastodon.access-token=""','--download-archive',dest/'archive.sqlite3','--write-metadata','-D',dest,'-f','{media[id]}.{extension}','mastodon:'+base+'/@fixture/'+ident]
    record=run('media-'+tool+'-'+ident,cmd,env=environment if tool=='gallery-dl' else None);records.append(record['exit'])
   files={p.stem:digest(p) for p in dest.rglob('*.png') if p.is_file()};media[tool]={'calls':len(records),'successful_calls':sum(x==0 for x in records),'correct_files':sum(files.get(k)==v for k,v in media_gold.items()),'expected_files':len(media_gold),'unexpected_files':sorted(set(files)-set(media_gold))}
  summary['media']=media;save(OUT/'summary.json',summary)
  stream_results={}
  for kind,ext in [('hls','m3u8'),('dash','mpd')]:
   dest=OUT/('stream-'+kind);result=run('stream-'+kind,[BIN,'--config',cfg,'stream-download',base+'/streams/'+kind+'/index.'+ext,'--out',dest],180)
   checks=[]
   if result['exit']==0:
    try:
     paths=[r['path'] for r in json.loads(result['stdout'])]
     for i,p in enumerate(paths):
      probe=run('probe-'+kind+'-'+str(i),['ffprobe','-v','error','-count_frames','-show_entries','stream=codec_type,codec_name,nb_read_frames','-of','json',p]);checks.append(json.loads(probe['stdout']))
    except Exception as e:checks.append({'error':str(e)})
   expected=run('probe-source-'+kind,['ffprobe','-v','error','-count_frames','-show_entries','stream=codec_type,codec_name,nb_read_frames','-of','json',streams/kind/('index.'+ext)])
   original=json.loads(expected['stdout']);flatten=lambda reports:sorted((s.get('codec_type'),s.get('codec_name'),s.get('nb_read_frames')) for report in reports for s in report.get('streams',[]))
   stream_results[kind]={'exit':result['exit'],'tracks':checks,'original':original,'decoded_frame_counts_match':bool(checks) and flatten(checks)==flatten([original])}
  summary['streams']=stream_results;save(OUT/'summary.json',summary)
 finally:server.shutdown();server.server_close()
 print(json.dumps(summary))

if __name__=='__main__':main()
