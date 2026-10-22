#!/usr/bin/env python3
"""Development regression audit; neither reused corpus is an unseen holdout."""
import importlib.util,json,statistics,sys
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
sys.path.insert(0,str(ROOT/'bench/fresh'))
from aeb import plain
OUT=Path(sys.argv[1]).resolve() if len(sys.argv)>1 else ROOT/'bench/six/results/editorial-core'
spec=importlib.util.spec_from_file_location('wcxb',ROOT/'bench/data/six/batch-next/wcxb/evaluate.py');wcxb=importlib.util.module_from_spec(spec);spec.loader.exec_module(wcxb)
gold=wcxb.load_ground_truth('test')
old={r['id']:r for r in map(json.loads,(ROOT/'bench/six/results/batch-next/current/results.jsonl').open())}
new={r['id']:r for r in map(json.loads,(OUT/'wcxb-final.jsonl').open())}
assert set(new)==set(old)==set(gold)
rows=[]
for k,g in gold.items():
 a=wcxb.word_f1(plain(old[k]['markdown']),g['main_content'])[2];b=wcxb.word_f1(plain(new[k]['markdown']),g['main_content'])[2]
 rows.append(dict(id=k,before=a,after=b,delta=b-a))
aeb=json.loads((OUT/'aeb-final/eval/results.json').read_text());truth=json.loads((ROOT/'bench/data/fresh/data/aeb/ground-truth.json').read_text());ids=list(truth)
f=lambda v:2*v[0]/(2*v[0]+v[1]+v[2]) if sum(v) else 0
losses=[]
for item in json.loads((ROOT/'bench/six/results/firecrawl-full-scores/diff.json').read_text())['pagesift_losses']:
 i=ids.index(item['id']);losses.append(dict(id=item['id'],site=item['site'],**{label:f(aeb[label]['tp_fp_fns'][i]) for label in ['final','sealed-aeb','firecrawl']}))
summary={'scope':'development regression, not unseen validation or six-project overall leadership','aeb_f1':{k:v['f1'] for k,v in aeb.items()},'prior_loss_cases':losses,'remaining_prior_losses':sum(r['final']<r['firecrawl'] for r in losses),'wcxb':{'n':len(rows),'before':statistics.mean(r['before'] for r in rows),'after':statistics.mean(r['after'] for r in rows),'improved':sum(r['delta']>1e-9 for r in rows),'declined':sum(r['delta']< -1e-9 for r in rows),'changes':sorted([r for r in rows if abs(r['delta'])>1e-9],key=lambda r:r['delta'])}}
(OUT/'audit.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary))
