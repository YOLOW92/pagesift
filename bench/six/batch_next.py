#!/usr/bin/env python3
"""Unseen WCXB test batch. Freeze before execution; never alter product rules."""
import concurrent.futures,gzip,hashlib,importlib.util,json,os,statistics,subprocess,sys,time
from pathlib import Path
from urllib.parse import urlsplit,urlunsplit,parse_qsl,urlencode
ROOT=Path(__file__).resolve().parents[2];DATA=ROOT/'bench/data/six/batch-next';OUT=ROOT/'bench/six/results/batch-next';REPO=DATA/'wcxb'
sys.path.insert(0,str(ROOT/'bench/fresh'));from aeb import plain
spec=importlib.util.spec_from_file_location('wcxb_evaluator',REPO/'evaluate.py');official=importlib.util.module_from_spec(spec);spec.loader.exec_module(official)
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def urlkey(u):
 p=urlsplit(u);return urlunsplit(('',(p.hostname or '').lower(),p.path.rstrip('/'),urlencode(sorted(parse_qsl(p.query))),''))
def prepare():
 OUT.mkdir(parents=True,exist_ok=False);(DATA/'html').mkdir(exist_ok=False)
 source=json.loads((ROOT/'bench/six/results/core-next/source.json').read_text());assert all(sha(ROOT/p)==h for p,h in source['source_hashes'].items())
 old_hashes=set();old_urls=set()
 for path in [ROOT/'bench/data/six/aeb.jsonl',ROOT/'bench/data/six/core-next/gold/manifest.jsonl']:
  for r in map(json.loads,path.open()):
   old_hashes.add(sha(Path(r['path'])));old_urls.add(urlkey(r.get('url','')))
 rows=[];overlap=[];metadata=json.loads((REPO/'metadata.json').read_text())['files']
 for g in sorted((REPO/'test/ground-truth').glob('*.json')):
  obj=json.loads(g.read_text());ident=g.stem;src=REPO/'test/html'/(ident+'.html.gz');raw=gzip.decompress(src.read_bytes());text=raw.decode('utf-8',errors='replace');dest=DATA/'html'/(ident+'.html');dest.write_text(text)
  duplicate=hashlib.sha256(raw).hexdigest() in old_hashes or urlkey(obj['url']) in old_urls
  row={'id':ident,'path':str(dest),'url':obj['url'],'domain':urlsplit(obj['url']).hostname,'page_type':official.get_page_type(obj),'spa':metadata.get(ident,{}).get('spa',False),'previous_input_overlap':duplicate,'raw_html_sha256':hashlib.sha256(raw).hexdigest(),'input_sha256':sha(dest),'gold_sha256':sha(g),'utf8_replacement':raw!=text.encode()};rows.append(row)
  if duplicate:overlap.append(ident)
 binaries={'previous':ROOT/'bench/data/six/core-next/baseline-content','current':ROOT/'bench/data/six/core-next/target/debug/examples/extract_jsonl','essence':ROOT/'bench/data/six/bin/offline-stable'}
 protocol={'dataset':'WCXB official test split; all pages, no output-based selection','upstream_sha':subprocess.check_output(['git','rev-parse','HEAD'],cwd=REPO,text=True).strip(),'license':'CC-BY-4.0','n':len(rows),'types':{t:sum(r['page_type']==t for r in rows) for t in sorted({r['page_type'] for r in rows})},'domains':len({r['domain'] for r in rows}),'spa':sum(r['spa'] for r in rows),'overlap_ids':overlap,'new_only_n':sum(not r['previous_input_overlap'] for r in rows),'metric':'official WCXB mean of per-page word F1; differs from earlier AEB 4-shingle harmonic macro metric','conversion':'fixed bench/fresh/aeb.py plain() for every Markdown output; same exact bytes per input across tools','per_document_timeout_s':10,'workers_per_tool':2,'per_tool_memory_mb':2048,'per_tool_cpu_quota':200,'failures':'empty prediction, retained in denominator; no replacements','candidate_snapshot':source['snapshot_sha'],'binaries':{k:{'path':str(p),'sha256':sha(p)} for k,p in binaries.items()},'scorer_sha256':sha(REPO/'evaluate.py'),'driver_sha256':sha(Path(__file__)),'product_source_hashes':source['source_hashes']}
 (OUT/'protocol.json').write_text(json.dumps(protocol,indent=2)+'\n');(OUT/'manifest.jsonl').write_text(''.join(json.dumps(r)+'\n' for r in rows));print(json.dumps({k:v for k,v in protocol.items() if k not in ('product_source_hashes','binaries')}))
def run_tool(label):
 protocol=json.loads((OUT/'protocol.json').read_text());rows=list(map(json.loads,(OUT/'manifest.jsonl').open()));binary=Path(protocol['binaries'][label]['path']);assert sha(binary)==protocol['binaries'][label]['sha256'];dest=OUT/label;dest.mkdir(exist_ok=False)
 def one(row):
  folder=dest/row['id'];folder.mkdir();manifest=folder/'input.jsonl';manifest.write_text(json.dumps({k:row[k] for k in ('id','path','url')})+'\n');output=folder/'output.jsonl';cmd=[str(binary)]+(['essence'] if label=='essence' else [])+[str(manifest),str(output)];start=time.monotonic();result={'id':row['id'],'status':'error','markdown':''}
  try:
   p=subprocess.run(cmd,capture_output=True,text=True,timeout=protocol['per_document_timeout_s']);(folder/'stderr.log').write_text(p.stderr);(folder/'stdout.log').write_text(p.stdout)
   if p.returncode:result.update(error=f'exit {p.returncode}')
   else:
    native=[json.loads(l) for l in output.open() if l.strip()];assert len(native)==1 and native[0]['id']==row['id'];r=native[0];result.update(markdown=r.get('markdown') or '',status='error' if r.get('error') else 'ok',native_ms=r.get('ms'),error=r.get('error'))
  except subprocess.TimeoutExpired as e:
   result.update(status='timeout',error='10s per-document budget');(folder/'stderr.log').write_bytes(e.stderr or b'');(folder/'stdout.log').write_bytes(e.stdout or b'')
  except Exception as e:result.update(error=repr(e))
  result['wall_s_including_startup']=time.monotonic()-start;return result
 with concurrent.futures.ThreadPoolExecutor(max_workers=protocol['workers_per_tool']) as pool:
  with (dest/'results.jsonl').open('w') as f:
   for r in pool.map(one,rows):f.write(json.dumps(r)+'\n');f.flush()
 print(json.dumps({'tool':label,'n':len(rows)}))
def score_all():
 protocol=json.loads((OUT/'protocol.json').read_text());manifest=list(map(json.loads,(OUT/'manifest.jsonl').open()));gold=official.load_ground_truth('test');summary={};details={}
 for tool in protocol['binaries']:
  rows={r['id']:r for r in map(json.loads,(OUT/tool/'results.jsonl').open())};assert set(rows)==set(gold);pred={k:plain(v['markdown']) for k,v in rows.items()};(OUT/tool/'predictions.json').write_text(json.dumps(pred));(OUT/tool/'scorer.log').write_text('')
  with (OUT/tool/'scorer.log').open('w') as log:
   p=subprocess.run([sys.executable,str(REPO/'evaluate.py'),'--split','test','--results',str(OUT/tool/'predictions.json'),'--per-type'],stdout=log,stderr=subprocess.STDOUT);assert p.returncode==0
  evaluated=[]
  for row in manifest:
   k=row['id'];g=gold[k];p,r,f=official.word_f1(pred[k],g['main_content']);evaluated.append(dict(id=k,page_type=row['page_type'],domain=row['domain'],new_only=not row['previous_input_overlap'],status=rows[k]['status'],precision=p,recall=r,f1=f,with_rate=official.snippet_check(pred[k],g['with']),without_rate=official.snippet_check(pred[k],g['without'])))
  def agg(values):return {'n':len(values),'precision':statistics.mean(v['precision'] for v in values),'recall':statistics.mean(v['recall'] for v in values),'f1':statistics.mean(v['f1'] for v in values)} if values else {'n':0}
  summary[tool]={'all':agg(evaluated),'new_only':agg([v for v in evaluated if v['new_only']]),'per_type':{t:agg([v for v in evaluated if v['page_type']==t]) for t in protocol['types']},'failures':[{'id':k,'status':v['status'],'error':v.get('error')} for k,v in rows.items() if v['status']!='ok']};details[tool]=evaluated;(OUT/tool/'scores.json').write_text(json.dumps(evaluated,indent=2)+'\n')
 assert all(sha(ROOT/p)==h for p,h in protocol['product_source_hashes'].items());(OUT/'summary.json').write_text(json.dumps({'protocol':{k:v for k,v in protocol.items() if k!='product_source_hashes'},'tools':summary,'product_unchanged':True},indent=2)+'\n');print(json.dumps(summary))
if __name__=='__main__':
 if sys.argv[1]=='prepare':prepare()
 elif sys.argv[1]=='run':run_tool(sys.argv[2])
 else:score_all()
