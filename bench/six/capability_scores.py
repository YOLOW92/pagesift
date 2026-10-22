#!/usr/bin/env python3
"""Reproduce body and business-identity capability checks; retain raw outputs."""
import hashlib, json, subprocess, sys
from pathlib import Path
ROOT = Path(__file__).resolve().parents[2]
out = Path(sys.argv[1]).resolve()
target = Path(sys.argv[2]).resolve()
out.mkdir(parents=True, exist_ok=False)
subprocess.run([str(target/'debug/examples/extract_jsonl'), str(ROOT/'bench/data/six/aeb.jsonl'), str(out/'candidate.jsonl')], check=True)
subprocess.run([sys.executable, str(ROOT/'bench/fresh/aeb.py'), 'score', str(ROOT/'bench/data/fresh/data/aeb'), str(out/'body'), str(out/'candidate.jsonl'), str(ROOT/'bench/six/results/firecrawl-full-domain/pagesift.jsonl'), str(ROOT/'bench/six/results/firecrawl-full-domain/firecrawl.jsonl')], check=True)
old = ROOT/'bench/six/results/round4/structured-final'
rows = [json.loads(line) for line in (old/'manifest.jsonl').open()]
original = (old/'original.html').read_text()
for ident, html, gold in [
    ('duplicate-business-id', original + original.replace('class="product"','class="different"'), None),
    ('updated-business-item', original.replace('Cost 73','Cost 109').replace('class="product"','class="changed"'), 'Independent Widget 東京 Cost 109')]:
    path=out/(ident+'.html');path.write_text(html)
    rows.append({'id':ident, 'path':str(path), 'selector':rows[0]['selector'], 'gold':gold})
manifest=out/'relocation.jsonl';manifest.write_text(''.join(json.dumps(r)+'\n' for r in rows))
binary=target/'debug/examples/relocate_jsonl'
subprocess.run([str(binary), str(old/'original.html'), rows[0]['selector'], str(manifest), str(out/'relocated.jsonl')], check=True)
pred={r['id']:r for r in map(json.loads,(out/'relocated.jsonl').open())}
norm=lambda values:[''.join(s.split()) for s in values]
details=[]
for r in rows:
    p=pred[r['id']];wanted=[] if r['gold'] is None else [r['gold']]
    details.append(dict(id=r['id'], gold=r['gold'], **{k:v for k,v in p.items() if k!='id'}, correct=norm(p['values'])==norm(wanted)))
scores=json.loads((out/'body/eval/results.json').read_text())
keys=list(json.loads((ROOT/'bench/data/fresh/data/aeb/ground-truth.json').read_text()))
f1=lambda t: 2*t[0]/(2*t[0]+t[1]+t[2]) if sum(t) else 0
losses=[]
for r in json.loads((ROOT/'bench/six/results/firecrawl-full-scores/diff.json').read_text())['pagesift_losses']:
    i=keys.index(r['id']);losses.append(dict(id=r['id'],site=r['site'],**{k:f1(scores[k]['tp_fp_fns'][i]) for k in scores}))
changes=[{'id':k,'before':f1(scores['pagesift']['tp_fp_fns'][i]),'after':f1(scores['candidate']['tp_fp_fns'][i])} for i,k in enumerate(keys) if abs(f1(scores['pagesift']['tp_fp_fns'][i])-f1(scores['candidate']['tp_fp_fns'][i]))>1e-9]
summary={'body_f1':{k:v['f1'] for k,v in scores.items()},'former_losses':losses,'changed_articles':changes,'relocation':{'n':len(details),'correct':sum(r['correct'] for r in details),'details':details,'scope':'synthetic fixed business identity required; not a population accuracy estimate'},'binaries':{str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in [binary,target/'debug/examples/extract_jsonl']}}
(out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
assert all(r['correct'] for r in details)
print(json.dumps({k:v for k,v in summary.items() if k not in ('former_losses','changed_articles','relocation','binaries')}))
