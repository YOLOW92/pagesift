#!/usr/bin/env python3
"""Frozen upstream Readability regression gold, with source-group holdout split."""
import hashlib,json,re,subprocess,sys
from pathlib import Path
from html.parser import HTMLParser
from urllib.parse import urlsplit
ROOT=Path(__file__).resolve().parents[2]
sys.path.insert(0,str(ROOT/'bench/fresh'))
from aeb import plain
import importlib.util
spec=importlib.util.spec_from_file_location('aeb_score',ROOT/'bench/data/fresh/data/aeb/evaluate.py');score=importlib.util.module_from_spec(spec);spec.loader.exec_module(score)
class Text(HTMLParser):
 def __init__(self): super().__init__();self.parts=[];self.skip=0;self.url=None
 def handle_starttag(self,tag,attrs):
  attrs=dict(attrs)
  if tag in ('script','style'):self.skip+=1
  if tag in ('p','div','section','article','h1','h2','h3','li','br','tr','pre'):self.parts.append(' ')
  if tag=='link' and attrs.get('rel')=='canonical':self.url=attrs.get('href')
  if tag=='meta' and attrs.get('property')=='og:url':self.url=attrs.get('content')
 def handle_endtag(self,tag):
  if tag in ('script','style'):self.skip=max(0,self.skip-1)
  if tag in ('p','div','section','article','h1','h2','h3','li','tr','pre'):self.parts.append(' ')
 def handle_data(self,data):
  if not self.skip:self.parts.append(data)
def prepare(dest):
 dest.mkdir(parents=True,exist_ok=False);source=ROOT/'bench/data/six/core-next/readability';rows=[];missing=[]
 for case in sorted((source/'test/test-pages').iterdir()):
  if not case.is_dir():continue
  html=case/'source.html';gold=case/'expected.html'
  if not html.exists() or not gold.exists():missing.append(case.name);continue
  p=Text();p.feed(html.read_text());host=urlsplit(p.url or '').hostname
  group=host if host and host not in ('fakehost','example.com','www.example.com') else re.sub(r'[-_]?\d+.*$','',case.name) or 'synthetic'
  split='holdout' if int(hashlib.sha256(('core-20261022:'+group).encode()).hexdigest()[:8],16)%10<4 else 'dev'
  if case.name=='001' or group=='synthetic':split='dev'
  url=p.url if p.url and p.url.startswith(('https://','http://')) else 'https://fixture.example/articles/'+case.name
  # Root-path base URLs would activate the homepage guard; preserve real inputs
  # as supplied rather than patching them to help an extractor.
  q=Text();q.feed(gold.read_text());truth=''.join(q.parts)
  rows.append({'id':case.name,'path':str(html),'url':url,'gold':truth,'group':group,'split':split,'source_sha256':hashlib.sha256(html.read_bytes()).hexdigest(),'gold_sha256':hashlib.sha256(gold.read_bytes()).hexdigest()})
 (dest/'gold.json').write_text(json.dumps(rows,indent=2))
 (dest/'manifest.jsonl').write_text(''.join(json.dumps({k:r[k] for k in ('id','path','url')})+'\n' for r in rows))
 protocol={'upstream_sha':subprocess.check_output(['git','rev-parse','HEAD'],cwd=source,text=True).strip(),'n':len(rows),'missing_pairs':missing,'split_counts':{s:sum(r['split']==s for r in rows) for s in ('dev','holdout')},'group_counts':{s:len({r['group'] for r in rows if r['split']==s}) for s in ('dev','holdout')},'preinspected_case':'001 forced into development','metric':'upstream AEB 4-shingle matching over expected HTML text and normalized candidate Markdown; macro P/R harmonic F1','purpose':'new third-party regression corpus, not population accuracy or an independent human blind study'}
 (dest/'protocol.json').write_text(json.dumps(protocol,indent=2)+'\n');print(json.dumps(protocol))
def evaluate(gold,output,dest,dev_only=False):
 dest.mkdir(parents=True,exist_ok=False);rows=json.loads((gold/'gold.json').read_text());rows=[r for r in rows if not dev_only or r['split']=='dev'];pred={r['id']:r for r in map(json.loads,output.open())};details=[]
 assert {r['id'] for r in rows} <= set(pred)
 for r in rows:
  t=score.string_shingle_matching(r['gold'],plain(pred[r['id']]['markdown']));p=score.precision_score(*t);rec=score.recall_score(*t)
  details.append({'id':r['id'],'split':r['split'],'group':r['group'],'tp_fp_fn':t,'precision':p,'recall':rec,'f1':2*p*rec/(p+rec) if p+rec else 0})
 summary={s:dict(n=sum(r['split']==s for r in details),**score.metrics_from_tp_fp_fns([r['tp_fp_fn'] for r in details if r['split']==s])) for s in (('dev',) if dev_only else ('dev','holdout'))}
 (dest/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');(dest/'dev.json').write_text(json.dumps([r for r in details if r['split']=='dev'],indent=2));(dest/'holdout.json').write_text(json.dumps([r for r in details if r['split']=='holdout'],indent=2));print(json.dumps(summary))
if __name__=='__main__':
 if sys.argv[1]=='prepare':prepare(Path(sys.argv[2]).resolve())
 else:evaluate(Path(sys.argv[2]).resolve(),Path(sys.argv[3]).resolve(),Path(sys.argv[4]).resolve(),sys.argv[1]=='score-dev')
