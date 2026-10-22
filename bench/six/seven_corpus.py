#!/usr/bin/env python3
"""New complete 545-page calibrated corpus; no output-based page selection.

Word F1 is our diagnostic, NOT the upstream five-part/ROUGE leaderboard score.
The extraction components and complete Firecrawl service are reported separately.
"""
import collections,hashlib,json,re,resource,subprocess,sys,time
from pathlib import Path
from lxml import html as lhtml
ROOT=Path(__file__).resolve().parents[2]
DATA=ROOT/'bench/data/six/seven-next'
OUT=ROOT/'bench/six/results/seven-next/content'
sys.path.insert(0,str(ROOT/'bench/fresh'))
from aeb import plain
from validate import validate

def sha(p):return hashlib.sha256(Path(p).read_bytes()).hexdigest()
def child_limits():resource.setrlimit(resource.RLIMIT_AS,(2*1024**3,2*1024**3))
def tokens(s):return re.findall(r'[\u3400-\u9fff]|[^\W_]+',s.casefold(),flags=re.UNICODE)
def f1(a,b):
 a,b=collections.Counter(tokens(a)),collections.Counter(tokens(b)); common=sum((a&b).values()); na,nb=sum(a.values()),sum(b.values())
 if not na and not nb:return 1.0
 return 2*common/(na+nb) if na+nb else 0.0

def prepare():
 OUT.mkdir(parents=True,exist_ok=False);(DATA/'html').mkdir(exist_ok=False);(DATA/'gold').mkdir(exist_ok=False)
 old_urls=set();old_hashes=set();prior_inputs=0
 for p in [ROOT/'bench/data/six/aeb.jsonl',ROOT/'bench/data/six/core-next/gold/manifest.jsonl',ROOT/'bench/six/results/batch-next/manifest.jsonl',ROOT/'bench/six/results/editorial-core/validation/manifest.jsonl']:
  if not p.exists():continue
  prior_inputs+=1
  for r in map(json.loads,p.open()):
   old_urls.add(r.get('url','').rstrip('/'))
   if r.get('path') and Path(r['path']).is_file():old_hashes.add(sha(r['path']))
 rows=[];annotations=0
 for r in map(json.loads,(DATA/'WebMainBench_545.jsonl').open()):
  ident=r['track_id'];raw=r['html'];url=r.get('url','');gold=r['groundtruth_content']
  # Remove human selection annotations from the actual DOM, not arbitrary
  # regex substrings inside scripts. Every extractor receives identical bytes.
  doc=lhtml.document_fromstring(raw)
  for node in doc.iter():
   for key in list(node.attrib):
    if key.startswith('cc-'):del node.attrib[key];annotations+=1
  cleaned=lhtml.tostring(doc,encoding='unicode',method='html')
  path=DATA/'html'/(ident+'.html');path.write_text(cleaned)
  g=DATA/'gold'/(ident+'.md');g.write_text(gold)
  overlap=(url.rstrip('/') in old_urls or hashlib.sha256(raw.encode()).hexdigest() in old_hashes) if prior_inputs==4 else None
  rows.append({'id':ident,'path':str(path),'url':url or 'https://fixture.example/'+ident,'gold_path':str(g),'sha256':sha(path),'gold_sha256':sha(g),'original_html_sha256':hashlib.sha256(raw.encode()).hexdigest(),'previous_overlap':overlap,'language':r.get('meta',{}).get('language')})
 assert len(rows)==545 and len({r['id'] for r in rows})==545
 (OUT/'manifest.jsonl').write_text(''.join(json.dumps(r,ensure_ascii=False)+'\n' for r in rows))
 frozen=json.loads((ROOT/'bench/six/evidence/2026-10-22/content/protocol.json').read_text())
 protocol={'dataset':'opendatalab/WebMainBench manually calibrated 545 subset, ALL 545 pages; not all 7809 pages','dataset_source':frozen['dataset_source'],'download':frozen['download'],'n':545,'previous_overlap':sum(r['previous_overlap'] is True for r in rows) if prior_inputs==4 else None,'no_previous_overlap':sum(r['previous_overlap'] is False for r in rows) if prior_inputs==4 else None,'annotations_removed':annotations,'preprocessing':'lxml DOM removal of all cc-* attributes and one common HTML serialization; gold never passed to extractors','metric':'mean per-page token multiset F1 of normalized visible Markdown text; CJK characters are separate tokens; our diagnostic, NOT official WebMainBench leaderboard','scorer_sha256':sha(__file__),'plain_converter_sha256':sha(ROOT/'bench/fresh/aeb.py'),'per_document_timeout_s':30,'errors':'empty output retained in denominator; no replacement or output-driven exclusion','references':json.loads((ROOT/'bench/six/tools.lock.json').read_text())['tools'],'scope_exclusions':['persistent browser interact lifecycle','agent autonomous data-gathering API'],'scope_exclusion_authority':'documented project scope','prior_inputs_available':prior_inputs,'overlap_check':'complete' if prior_inputs==4 else 'not reproduced; do not claim novel pages'}
 (OUT/'protocol.json').write_text(json.dumps(protocol,indent=2)+'\n');print(json.dumps({k:protocol[k] for k in ['n','previous_overlap','no_previous_overlap','annotations_removed']}))

def run(tool,binary):
 rows=list(map(json.loads,(OUT/'manifest.jsonl').open()));dest=OUT/tool;dest.mkdir(exist_ok=False)
 (dest/'binary.json').write_text(json.dumps({'path':str(binary),'sha256':sha(binary)},indent=2)+'\n')
 # Same original-URL context for the two offline components. Service replay
 # scores use a separate shared replay URL context and are not mixed with these.
 with (dest/'results.jsonl').open('w') as output:
  for i,r in enumerate(rows):
   m=dest/'one.jsonl';m.write_text(json.dumps(r)+'\n');p=dest/'one-output.jsonl';p.unlink(missing_ok=True);start=time.monotonic()
   try:
    cmd=[str(binary)]+(['essence'] if tool=='essence-component' else [])+[str(m),str(p)]
    result=subprocess.run(cmd,capture_output=True,text=True,timeout=30,preexec_fn=child_limits)
    if result.returncode:raise RuntimeError(result.stderr[-2000:])
    record=json.loads(p.read_text().splitlines()[0]);record['id']=r['id']
   except Exception as e:record={'id':r['id'],'status':'error','markdown':'','error':str(e)}
   record['wall_s']=time.monotonic()-start;output.write(json.dumps(record,ensure_ascii=False)+'\n');output.flush()
   if (i+1)%100==0:print(json.dumps({'tool':tool,'done':i+1}),flush=True)
 (dest/'summary.json').write_text(json.dumps(score(dest/'results.jsonl'),indent=2)+'\n')

def score(path):
 validate(OUT/'manifest.jsonl',path)
 manifest=list(map(json.loads,(OUT/'manifest.jsonl').open()));pred={r['id']:r for r in map(json.loads,path.open())};assert set(pred)=={r['id'] for r in manifest}
 details=[]
 for r in manifest:
  v=pred[r['id']];text=v.get('markdown') or '';truth=Path(r['gold_path']).read_text()
  details.append({'id':r['id'],'f1':f1(plain(text),plain(truth)),'error':v.get('status','ok')!='ok','previous_overlap':r['previous_overlap'],'language':r['language']})
 result={'n':len(details),'f1':sum(r['f1'] for r in details)/len(details),'errors':sum(r['error'] for r in details)}
 fresh=[r for r in details if r['previous_overlap'] is False];result['new_pages']={'n':len(fresh),'f1':sum(r['f1'] for r in fresh)/len(fresh) if fresh else None}
 path.with_name('scores.json').write_text(json.dumps(details,indent=2)+'\n');print(json.dumps(result));return result

if __name__=='__main__':
 if sys.argv[1]=='prepare':prepare()
 elif sys.argv[1]=='run':run(sys.argv[2],Path(sys.argv[3]).resolve())
 else:score(Path(sys.argv[2]).resolve())
