#!/usr/bin/env python3
"""Audit the sealed candidate and report grouped paired uncertainty."""
import hashlib,json,random,re,subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2];OUT=ROOT/'bench/six/results/core-next'
source=json.loads((OUT/'source.json').read_text());mismatch=[n for n,h in source['source_hashes'].items() if hashlib.sha256((ROOT/n).read_bytes()).hexdigest()!=h];assert not mismatch
old=json.loads((OUT/'gold-baseline/holdout.json').read_text());new=json.loads((OUT/'sealed-validation/holdout.json').read_text());reference=json.loads((OUT/'essence-validation/holdout.json').read_text());index={r['id']:r for r in old};ref={r['id']:r for r in reference};groups={}
for r in new:groups.setdefault(r['group'],[]).append(r['id'])
new_idx={r['id']:r for r in new};keys=list(groups);rng=random.Random(20261022)
def macro(rows):
 p=sum(r['precision'] for r in rows)/len(rows);rec=sum(r['recall'] for r in rows)/len(rows);return 2*p*rec/(p+rec) if p+rec else 0
sample=[];versus=[]
for _ in range(2000):
 ids=[ident for _ in keys for ident in groups[rng.choice(keys)]];sample.append(macro([new_idx[i] for i in ids])-macro([index[i] for i in ids]));versus.append(macro([new_idx[i] for i in ids])-macro([ref[i] for i in ids]))
sample.sort();versus.sort();uncertainty={'scope':'exploratory paired source-group bootstrap, not a preregistered significance test','seed':20261022,'repetitions':2000,'groups':len(groups),'candidate_minus_previous_95_percentile':[sample[50],sample[1949]],'candidate_minus_essence_component_95_percentile':[versus[50],versus[1949]]}
proto=json.loads((ROOT/'bench/data/six/core-next/gold/protocol.json').read_text());gold=json.loads((ROOT/'bench/data/six/core-next/gold/gold.json').read_text());dev={r['group'] for r in gold if r['split']=='dev'};held={r['group'] for r in gold if r['split']=='holdout'};assert not dev&held
prior_hashes={r.get('sha256') for r in map(json.loads,(ROOT/'bench/data/six/aeb.jsonl').open())};duplicates=[r['id'] for r in gold if r['source_sha256'] in prior_hashes]
aeb=json.loads((OUT/'sealed-aeb-score/eval/results.json').read_text());truth=json.loads((ROOT/'bench/data/fresh/data/aeb/ground-truth.json').read_text());ids=list(truth);f=lambda v:2*v[0]/(2*v[0]+v[1]+v[2]) if sum(v) else 0
changes=[]
for i,k in enumerate(ids):
 before=f(aeb['candidate']['tp_fp_fns'][i]);after=f(aeb['sealed-aeb']['tp_fp_fns'][i]);
 if abs(after-before)>1e-9:changes.append({'id':k,'url':truth[k]['url'],'before':before,'after':after})
losses=[]
for r in json.loads((ROOT/'bench/six/results/firecrawl-full-scores/diff.json').read_text())['pagesift_losses']:
 i=ids.index(r['id']);losses.append({'id':r['id'],'site':r['site'],**{k:f(aeb[k]['tp_fp_fns'][i]) for k in ('candidate','sealed-aeb','firecrawl')}})
ci=sorted((ROOT/'bench/six/results').glob('*-core-next-full-ci-*/run.json'))[-1];record=json.loads(ci.read_text());assert record['status']=='completed';text=ci.with_name('stdout.log').read_text();tests=sum(map(int,re.findall(r'test result: ok\. (\d+) passed',text)))
live=OUT/'live-migration';events=[json.loads(l) for l in (live/'events.jsonl').open()];assert [r['id'] for r in events[:2]]==['1','2'] and len(events)==3 and events[2]['id'] not in ('1','2')
summary={'status':'bounded_core_improvements_verified_not_global_parity','source_snapshot':source['snapshot_sha'],'source_matches_snapshot':True,'tests':tests,'ci_record':str(ci.relative_to(ROOT)),'gold_protocol':proto,'group_overlap':[],'exact_source_duplicates_with_aeb':duplicates,'new_gold_previous':json.loads((OUT/'gold-baseline/summary.json').read_text()),'new_gold_candidate':json.loads((OUT/'sealed-validation/summary.json').read_text()),'new_gold_essence_component':json.loads((OUT/'essence-validation/summary.json').read_text()),'uncertainty':uncertainty,'aeb_f1':{k:v['f1'] for k,v in aeb.items()},'aeb_changes':changes,'former_losses':losses,'former_losses_remaining':sum(r['sealed-aeb']<r['firecrawl'] for r in losses),'relocation':json.loads((OUT/'relocation/summary.json').read_text()),'checkpoint_scale':json.loads((OUT/'checkpoint-volume/summary.json').read_text()),'live_legacy_resume_event_ids':[r['id'] for r in events]}
(OUT/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps({k:summary[k] for k in ('status','tests','source_snapshot','former_losses_remaining','uncertainty')}))
