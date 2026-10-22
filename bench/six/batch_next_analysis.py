#!/usr/bin/env python3
"""Paired source-group uncertainty and input-validity sensitivity; no retuning."""
import collections,hashlib,json,random,statistics
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2];OUT=ROOT/'bench/six/results/batch-next'
summary=json.loads((OUT/'summary.json').read_text());manifest={r['id']:r for r in map(json.loads,(OUT/'manifest.jsonl').open())};scores={t:{r['id']:r for r in json.loads((OUT/t/'scores.json').read_text())} for t in summary['tools']};groups={}
for k,r in manifest.items():groups.setdefault(r['domain'] or 'missing-url:'+k,[]).append(k)
rng=random.Random(20261022);bootstrap={}
for other in ['previous','essence']:
 aggregates=[(len(ids),sum(scores['current'][i]['f1']-scores[other][i]['f1'] for i in ids)) for ids in groups.values()];samples=[]
 for _ in range(2000):
  draws=[rng.choice(aggregates) for _ in aggregates];samples.append(sum(d for n,d in draws)/sum(n for n,d in draws))
 samples.sort();bootstrap[other]={'difference':summary['tools']['current']['all']['f1']-summary['tools'][other]['all']['f1'],'source_group_bootstrap_95_percentile':[samples[50],samples[1949]]}
valid=[k for k,r in manifest.items() if r['url'].startswith(('http://','https://'))];valid_scores={t:{'n':len(valid),'f1':statistics.mean(scores[t][k]['f1'] for k in valid)} for t in scores};deltas=[{'id':k,'url':manifest[k]['url'],'page_type':manifest[k]['page_type'],'before':scores['previous'][k]['f1'],'after':scores['current'][k]['f1']} for k in manifest];changed=[r for r in deltas if abs(r['after']-r['before'])>1e-9]
analysis={'scope':'post-run descriptive analysis, no product changes or output-driven resampling','bootstrap':{'seed':20261022,'repetitions':2000,'groups':len(groups),'comparisons':bootstrap},'valid_absolute_url_sensitivity':{'note':'post-hoc input validity sensitivity, primary 511-page score unchanged; no rows replaced','excluded_ids':[k for k in manifest if k not in valid],'tools':valid_scores},'previous_to_current':{'improved':sum(r['after']>r['before'] for r in changed),'declined':sum(r['after']<r['before'] for r in changed),'unchanged':len(manifest)-len(changed),'changes':sorted(changed,key=lambda r:r['after']-r['before'])},'lowest_current_cases':[dict(id=k,url=manifest[k]['url'],page_type=manifest[k]['page_type'],f1=r['f1'],precision=r['precision'],recall=r['recall']) for k,r in sorted(scores['current'].items(),key=lambda kv:kv[1]['f1'])[:20]],'valid_hostnames':len({r['domain'] for r in manifest.values() if r['domain']}),'missing_url_pages':sum(not r['url'] for r in manifest.values())}
(OUT/'analysis.json').write_text(json.dumps(analysis,indent=2)+'\n');print(json.dumps({k:v for k,v in analysis.items() if k not in ('lowest_current_cases','previous_to_current')}))
