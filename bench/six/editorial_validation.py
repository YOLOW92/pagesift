#!/usr/bin/env python3
"""Freeze remaining WCXB dev pages before first candidate output; no resampling."""
import gzip,hashlib,importlib.util,json,statistics,subprocess,sys
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2];REPO=ROOT/'bench/data/six/batch-next/wcxb';OUT=ROOT/'bench/six/results/editorial-core/validation';DATA=ROOT/'bench/data/six/editorial-validation'
sys.path.insert(0,str(ROOT/'bench/fresh'));from aeb import plain
spec=importlib.util.spec_from_file_location('wcxb',REPO/'evaluate.py');w=importlib.util.module_from_spec(spec);spec.loader.exec_module(w)
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
if sys.argv[1]=='prepare':
 OUT.mkdir(exist_ok=False);DATA.mkdir(exist_ok=False)
 seen_hash=set();seen_url=set()
 for manifest in [ROOT/'bench/data/six/aeb.jsonl',ROOT/'bench/data/six/core-next/gold/manifest.jsonl',ROOT/'bench/six/results/batch-next/manifest.jsonl']:
  for r in map(json.loads,manifest.open()):seen_hash.add(sha(Path(r['path'])));seen_url.add(r.get('url','').rstrip('/'))
 rows=[];excluded=[]
 for g in sorted((REPO/'dev/ground-truth').glob('*.json')):
  obj=json.loads(g.read_text());raw=gzip.decompress((REPO/'dev/html'/(g.stem+'.html.gz')).read_bytes());digest=hashlib.sha256(raw).hexdigest()
  if digest in seen_hash or obj['url'].rstrip('/') in seen_url:excluded.append(g.stem);continue
  p=DATA/(g.stem+'.html');p.write_bytes(raw)
  rows.append(dict(id=g.stem,path=str(p),url=obj['url'],sha256=digest,gold_sha256=sha(g)))
 binaries={'before':ROOT/'bench/data/six/core-next/target/debug/examples/extract_jsonl','after':ROOT/'bench/data/six/editorial-target/debug/examples/extract_jsonl','essence':ROOT/'bench/data/six/bin/offline-stable'}
 files=[p for p in (ROOT/'crates').rglob('*') if p.is_file() and 'target' not in p.parts]
 protocol={'scope':'first use of remaining upstream development split; independent pages, shared dataset and potentially shared domains; not official held-out test','n':len(rows),'excluded_exact_input_or_url_overlap':excluded,'dataset_commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=REPO,text=True).strip(),'binaries':{k:{'path':str(p),'sha256':sha(p)} for k,p in binaries.items()},'product_hashes':{str(p.relative_to(ROOT)):sha(p) for p in files},'metric':'official mean per-page word F1; same plain() converter','selection':'all remaining dev pages; no output-based selection','per_document_timeout_s':10,'workers_per_tool':2}
 (OUT/'manifest.jsonl').write_text(''.join(json.dumps(r)+'\n' for r in rows));(OUT/'protocol.json').write_text(json.dumps(protocol,indent=2)+'\n');print(json.dumps({'n':len(rows),'excluded':excluded}))
elif sys.argv[1]=='run':
 sys.path.insert(0,str(ROOT/'bench/six'));import batch_next as driver
 protocol=json.loads((OUT/'protocol.json').read_text());label=sys.argv[2];driver.OUT=OUT;driver.run_tool(label)
else:
 protocol=json.loads((OUT/'protocol.json').read_text());gold=w.load_ground_truth('dev');manifest=list(map(json.loads,(OUT/'manifest.jsonl').open()));summary={}
 assert all(sha(ROOT/p)==h for p,h in protocol['product_hashes'].items())
 for label in protocol['binaries']:
  rows={r['id']:r for r in map(json.loads,(OUT/label/'results.jsonl').open())};assert set(rows)=={r['id'] for r in manifest};scores=[]
  for r in manifest:
   k=r['id'];p,rec,f=w.word_f1(plain(rows[k]['markdown']),gold[k]['main_content']);scores.append(dict(id=k,precision=p,recall=rec,f1=f,page_type=w.get_page_type(gold[k])))
  summary[label]={'n':len(scores),'f1':statistics.mean(r['f1'] for r in scores),'precision':statistics.mean(r['precision'] for r in scores),'recall':statistics.mean(r['recall'] for r in scores),'errors':[k for k,r in rows.items() if r['status']!='ok']};(OUT/label/'scores.json').write_text(json.dumps(scores,indent=2)+'\n')
 (OUT/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary))
